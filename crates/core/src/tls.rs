//! Extra trust anchors for outbound TLS.

use std::fmt;
use std::sync::Arc;

use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;

/// Certificates that outbound TLS clients trust in addition to their built-in roots, such as a
/// private CA of a self-hosted server.
///
/// Adding roots never turns verification off: a server still needs a certificate for its name
/// that chains to a built-in root or to one of these. The default holds none, which leaves every
/// client with its built-in roots only. Cloning is cheap.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TlsRoots(Arc<[Vec<u8>]>);

/// A PEM file that holds no certificate, or a certificate section that does not decode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidRoots(String);

impl TlsRoots {
    /// Every `CERTIFICATE` section of a PEM file, in file order; other sections such as keys
    /// are skipped. `InvalidRoots` when a section does not decode or there is no certificate.
    pub fn from_pem(pem: &[u8]) -> Result<Self, InvalidRoots> {
        let certs = CertificateDer::pem_slice_iter(pem)
            .map(|cert| cert.map(|cert| cert.as_ref().to_vec()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| InvalidRoots(format!("not a PEM certificate file: {e}")))?;
        if certs.is_empty() {
            return Err(InvalidRoots("no certificate in the PEM file".into()));
        }
        Ok(Self(certs.into()))
    }

    /// Certificates in DER form, kept as given; clients check them when they are built.
    pub fn from_der(certs: Vec<Vec<u8>>) -> Self {
        Self(certs.into())
    }

    /// Whether there are no extra roots.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many extra roots there are.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The certificates in DER form, in the order they were given.
    pub fn iter(&self) -> impl Iterator<Item = &[u8]> {
        self.0.iter().map(Vec::as_slice)
    }
}

impl fmt::Debug for TlsRoots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TlsRoots({} certificates)", self.0.len())
    }
}

/// A throwaway CA and TLS servers that use it, for tests of clients that take [`TlsRoots`].
#[cfg(feature = "testing")]
pub mod testing {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
        KeyUsagePurpose,
    };
    use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;
    use tokio_rustls::rustls::ServerConfig;

    use super::TlsRoots;

    /// A new CA and one server certificate it signed.
    pub struct TestCa {
        ca: Vec<u8>,
        ca_pem: String,
        server: CertificateDer<'static>,
        key: Vec<u8>,
    }

    impl TestCa {
        /// A CA and a certificate for `names` (host names or IP addresses), valid from a day ago
        /// for thirty days.
        pub fn new(names: &[&str]) -> Self {
            let now = std::time::SystemTime::now();
            let day = std::time::Duration::from_secs(86_400);
            let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("params");
            ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            ca_params
                .distinguished_name
                .push(rcgen::DnType::CommonName, "Holtmail test CA");
            ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
            ca_params.not_before = (now - day).into();
            ca_params.not_after = (now + 30 * day).into();
            let ca_key = KeyPair::generate().expect("CA key");
            let ca = ca_params.self_signed(&ca_key).expect("CA certificate");
            let issuer = Issuer::new(ca_params, ca_key);

            let mut params =
                CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>())
                    .expect("server names");
            params.distinguished_name.push(
                rcgen::DnType::CommonName,
                names.first().copied().unwrap_or("server"),
            );
            params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
            params.not_before = (now - day).into();
            params.not_after = (now + 30 * day).into();
            let key = KeyPair::generate().expect("server key");
            let server = params.signed_by(&key, &issuer).expect("server certificate");
            Self {
                ca: ca.der().to_vec(),
                ca_pem: ca.pem(),
                server: server.der().clone(),
                key: key.serialize_der(),
            }
        }

        /// The CA as extra roots.
        pub fn roots(&self) -> TlsRoots {
            TlsRoots::from_der(vec![self.ca.clone()])
        }

        /// The CA certificate as PEM.
        pub fn ca_pem(&self) -> &str {
            &self.ca_pem
        }

        /// A TLS acceptor that presents the server certificate.
        pub fn acceptor(&self) -> TlsAcceptor {
            let config = ServerConfig::builder_with_provider(Arc::new(
                tokio_rustls::rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("TLS versions")
            .with_no_client_auth()
            .with_single_cert(
                vec![self.server.clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone())),
            )
            .expect("server certificate and key");
            TlsAcceptor::from(Arc::new(config))
        }

        /// Serves HTTPS on a loopback port until the runtime stops. Every request, whatever its
        /// method and path, gets `status` with a JSON `body` and `Connection: close`.
        pub async fn serve_https(&self, status: u16, body: &'static str) -> SocketAddr {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let address = listener.local_addr().expect("address");
            let acceptor = self.acceptor();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let acceptor = acceptor.clone();
                    tokio::spawn(async move {
                        let Ok(mut stream) = acceptor.accept(stream).await else {
                            return;
                        };
                        let mut request = Vec::new();
                        let mut buf = [0u8; 4096];
                        let body_start = loop {
                            match stream.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => request.extend_from_slice(&buf[..n]),
                            }
                            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                                break end + 4;
                            }
                        };
                        let head = String::from_utf8_lossy(&request[..body_start]).to_lowercase();
                        let length = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        while request.len() < body_start + length {
                            match stream.read(&mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => request.extend_from_slice(&buf[..n]),
                            }
                        }
                        let response = format!(
                            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes()).await;
                        let _ = stream.shutdown().await;
                    });
                }
            });
            address
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEM: &str = "-----BEGIN PRIVATE KEY-----\nAAEC\n-----END PRIVATE KEY-----\n-----BEGIN CERTIFICATE-----\nAAEC\n-----END CERTIFICATE-----\ntext between\n-----BEGIN CERTIFICATE-----\nAwQF\n-----END CERTIFICATE-----\n";

    #[test]
    fn reads_every_certificate_and_skips_keys() {
        let roots = TlsRoots::from_pem(PEM.as_bytes()).unwrap();
        assert_eq!(roots.len(), 2);
        assert_eq!(
            roots.iter().collect::<Vec<_>>(),
            vec![&[0u8, 1, 2][..], &[3, 4, 5][..]]
        );
        assert_eq!(format!("{roots:?}"), "TlsRoots(2 certificates)");
    }

    #[test]
    fn refuses_files_without_certificates() {
        assert!(TlsRoots::from_pem(b"").is_err());
        assert!(TlsRoots::from_pem(b"not pem at all").is_err());
        assert!(
            TlsRoots::from_pem(b"-----BEGIN PRIVATE KEY-----\nAAEC\n-----END PRIVATE KEY-----\n")
                .is_err()
        );
        assert!(
            TlsRoots::from_pem(b"-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n")
                .is_err()
        );
    }

    #[test]
    fn defaults_to_no_roots() {
        assert!(TlsRoots::default().is_empty());
        assert_eq!(TlsRoots::default().iter().count(), 0);
    }
}
