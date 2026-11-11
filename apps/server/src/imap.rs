//! IMAP for mail clients: the `[serve.imap]` listeners over the served mail view, started only
//! when that section is there, with the change hub feeding `IDLE`.

use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use holtmail_core::{ArchiveId, DeviceProtocol, UserId};
use holtmail_device::DeviceAuth;
use holtmail_mailview::{MailActions, MailView};
use holtmail_serve_imap::{ChangeListener, ChangeSignal, ImapServer, PortTls, Settings};
use holtmail_serve_net::{ConnectionLimiter, Limits, TlsCertificates};
use tokio::net::TcpListener;

use crate::config::Config;
use crate::mailview::{ChangeHub, HubListener};

/// The change hub as the IMAP server's wake-ups.
pub struct HubSignal(pub Arc<ChangeHub>);

impl ChangeSignal for HubSignal {
    fn listen(&self, archive: ArchiveId, user: UserId) -> Box<dyn ChangeListener> {
        Box::new(self.0.subscribe(archive, user))
    }
}

#[async_trait]
impl ChangeListener for HubListener {
    async fn changed(&mut self) -> Option<u64> {
        HubListener::changed(self).await
    }
}

/// The ports `[serve.imap]` listens on, implicit TLS first.
pub fn endpoints(config: &Config) -> Vec<holtmail_api::ServeEndpoint> {
    let Some(section) = &config.serve.imap else {
        return Vec::new();
    };
    crate::submit::endpoint_rows(
        DeviceProtocol::Imap,
        crate::submit::configured_host(section.hostname.as_deref(), config),
        &section.listen_tls,
        &section.listen_starttls,
    )
}

/// What one `start` needs besides the config.
pub struct Parts {
    pub view: Arc<dyn MailView>,
    pub actions: Arc<dyn MailActions>,
    pub devices: Arc<dyn DeviceAuth>,
    pub hub: Arc<ChangeHub>,
    pub instance_token: String,
}

/// Binds every `[serve.imap]` listener and serves it until the process ends; does nothing
/// without that section.
pub async fn start(
    config: &Config,
    parts: Parts,
    certificates: Option<&TlsCertificates>,
) -> anyhow::Result<()> {
    let Some(section) = &config.serve.imap else {
        return Ok(());
    };
    let settings = Settings {
        instance_token: Some(parts.instance_token),
        ..Settings::default()
    };
    let server = ImapServer::new(
        parts.view,
        parts.actions,
        parts.devices,
        Arc::new(HubSignal(parts.hub)),
        settings,
    );
    let trusted = config.serve.validate()?;
    let limiter = ConnectionLimiter::new(Limits::default());
    let mut ports = Vec::new();
    for address in &section.listen_tls {
        let certificates = certificates
            .context("[serve.imap] listen_tls needs [serve] tls_cert")?
            .clone();
        ports.push((*address, PortTls::Implicit(certificates)));
    }
    for address in &section.listen_starttls {
        ports.push((*address, PortTls::Starttls(certificates.cloned())));
    }
    for (address, tls) in ports {
        let listener = TcpListener::bind(address)
            .await
            .with_context(|| format!("[serve.imap] listening on {address}"))?;
        tracing::info!(%address, "IMAP");
        tokio::spawn(server.clone().serve(
            listener,
            tls,
            config.serve.proxy_protocol,
            trusted.clone(),
            limiter.clone(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_name_the_imap_ports() {
        let off: Config = toml::from_str("").unwrap();
        assert!(endpoints(&off).is_empty());
        let config: Config = toml::from_str(
            "public_url = \"https://mail.example.com/\"\n[serve.imap]\nlisten_tls = [\"0.0.0.0:993\"]\nlisten_starttls = [\"0.0.0.0:143\", \"[::]:143\"]",
        )
        .unwrap();
        let rows: Vec<_> = endpoints(&config)
            .into_iter()
            .map(|r| (r.protocol, r.host, r.port, r.implicit_tls))
            .collect();
        let host = Some("mail.example.com".to_owned());
        assert_eq!(
            rows,
            [
                (DeviceProtocol::Imap, host.clone(), 993, true),
                (DeviceProtocol::Imap, host, 143, false),
            ]
        );
    }
}
