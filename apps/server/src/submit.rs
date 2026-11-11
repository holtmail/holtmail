//! SMTP submission: the [`Submission`] every front end sends through, and the `[serve.submit]`
//! listeners, started only when that section is there.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use holtmail_access::Access;
use holtmail_core::DeviceProtocol;
use holtmail_device::DeviceAuth;
use holtmail_outbox::Outbox;
use holtmail_serve_net::{ConnectionLimiter, Limits, TlsCertificates};
use holtmail_serve_submit::{PortTls, Settings, SubmitServer};
use holtmail_store::{SettingsStore, Store};
use holtmail_submission::Submission;
use holtmail_submission_outbox::{Options, OutboxSubmission};
use tokio::net::TcpListener;

use crate::config::{Config, Services, SubmitConfig};

const INSTANCE_TOKEN: &str = "instance_token";

/// This instance's token, made once and kept in the database, so a restored backup is still
/// the same instance. It says nothing about the instance beyond telling it from another.
pub async fn instance_token(store: &dyn Store) -> anyhow::Result<String> {
    if let Some(token) = SettingsStore::setting(store, INSTANCE_TOKEN).await? {
        return Ok(token);
    }
    let token = blake3::hash(uuid::Uuid::new_v4().as_bytes())
        .to_hex()
        .to_string();
    SettingsStore::set_setting(store, INSTANCE_TOKEN, Some(&token)).await?;
    Ok(token)
}

/// The one way into the outbox for people and devices.
pub fn submission(
    services: &Services,
    config: &Config,
    outbox: Arc<Outbox>,
) -> Arc<dyn Submission> {
    let limits = config.serve.submit.as_ref();
    let defaults = Options::default();
    Arc::new(OutboxSubmission::new(
        services.store.clone(),
        services.blobs.clone(),
        Access::new(services.store.clone(), config.write.into()),
        outbox,
        Options {
            messages_per_hour: limits.map_or(defaults.messages_per_hour, |l| l.messages_per_hour),
            recipients_per_day: limits
                .map_or(defaults.recipients_per_day, |l| l.recipients_per_day),
        },
    ))
}

/// The host clients are told to use: the section's own, else the host of `public_url`.
pub fn configured_host(hostname: Option<&str>, config: &Config) -> Option<String> {
    hostname.map(str::to_owned).or_else(|| {
        let url = config.public_url.as_deref()?;
        url::Url::parse(url).ok()?.host_str().map(str::to_owned)
    })
}

fn hostname(section: &SubmitConfig, config: &Config) -> String {
    configured_host(section.hostname.as_deref(), config).unwrap_or_else(|| "localhost".to_owned())
}

/// One row for each port and kind of TLS, implicit TLS first and in ascending port order
/// within a kind.
pub fn endpoint_rows(
    protocol: DeviceProtocol,
    host: Option<String>,
    listen_tls: &[SocketAddr],
    listen_starttls: &[SocketAddr],
) -> Vec<holtmail_api::ServeEndpoint> {
    let ports = |addresses: &[SocketAddr]| {
        addresses
            .iter()
            .map(SocketAddr::port)
            .collect::<BTreeSet<_>>()
    };
    let row = |port, implicit_tls| holtmail_api::ServeEndpoint {
        protocol,
        host: host.clone(),
        port,
        implicit_tls,
        url: None,
    };
    ports(listen_tls)
        .into_iter()
        .map(|port| row(port, true))
        .chain(
            ports(listen_starttls)
                .into_iter()
                .map(|port| row(port, false)),
        )
        .collect()
}

/// The ports `[serve.submit]` listens on, as [`endpoint_rows`].
pub fn endpoints(config: &Config) -> Vec<holtmail_api::ServeEndpoint> {
    let Some(section) = &config.serve.submit else {
        return Vec::new();
    };
    endpoint_rows(
        DeviceProtocol::Smtp,
        configured_host(section.hostname.as_deref(), config),
        &section.listen_tls,
        &section.listen_starttls,
    )
}

/// Binds every `[serve.submit]` listener and serves it until the process ends; does nothing
/// without that section.
pub async fn start(
    config: &Config,
    submission: Arc<dyn Submission>,
    devices: Arc<dyn DeviceAuth>,
    certificates: Option<&TlsCertificates>,
    instance_token: String,
    scheduler: Arc<dyn holtmail_schedule::Scheduler>,
) -> anyhow::Result<()> {
    let Some(section) = &config.serve.submit else {
        return Ok(());
    };
    let mut settings = Settings::new(hostname(section, config));
    settings.instance_token = Some(instance_token);
    settings.undo_window = Duration::from_secs(section.undo_seconds);
    let server = SubmitServer::with_scheduler(submission, devices, settings, scheduler);
    let trusted = config.serve.validate()?;
    let limiter = ConnectionLimiter::new(Limits::default());
    let mut ports = Vec::new();
    for address in &section.listen_tls {
        let certificates = certificates
            .context("[serve.submit] listen_tls needs [serve] tls_cert")?
            .clone();
        ports.push((*address, PortTls::Implicit(certificates)));
    }
    for address in &section.listen_starttls {
        ports.push((*address, PortTls::Starttls(certificates.cloned())));
    }
    for (address, tls) in ports {
        let listener = TcpListener::bind(address)
            .await
            .with_context(|| format!("[serve.submit] listening on {address}"))?;
        tracing::info!(%address, "SMTP submission");
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

    fn row(host: Option<&str>, port: u16, implicit_tls: bool) -> holtmail_api::ServeEndpoint {
        holtmail_api::ServeEndpoint {
            protocol: DeviceProtocol::Smtp,
            host: host.map(str::to_owned),
            port,
            implicit_tls,
            url: None,
        }
    }

    #[test]
    fn endpoints_name_each_port_once_with_tls_first() {
        let off: Config = toml::from_str("").unwrap();
        assert!(endpoints(&off).is_empty());

        let config: Config = toml::from_str(
            "public_url = \"https://mail.example.com/\"\n[serve.submit]\nlisten_tls = [\"0.0.0.0:465\", \"[::]:465\"]\nlisten_starttls = [\"0.0.0.0:587\"]",
        )
        .unwrap();
        let host = Some("mail.example.com");
        assert_eq!(
            endpoints(&config),
            [row(host, 465, true), row(host, 587, false)]
        );

        let named: Config = toml::from_str(
            "public_url = \"https://mail.example.com/\"\n[serve.submit]\nlisten_starttls = [\"127.0.0.1:2587\"]\nhostname = \"smtp.example.org\"",
        )
        .unwrap();
        assert_eq!(
            endpoints(&named),
            [row(Some("smtp.example.org"), 2587, false)]
        );

        let unnamed: Config =
            toml::from_str("[serve.submit]\nlisten_starttls = [\"127.0.0.1:2587\"]").unwrap();
        assert_eq!(endpoints(&unnamed), [row(None, 2587, false)]);
    }
}
