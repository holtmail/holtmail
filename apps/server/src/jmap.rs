//! JMAP for mail clients: the `[serve.jmap]` routes on the main listener over the served mail
//! view, with the change hub feeding push.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use axum::Router;
use holtmail_blob::BlobStore;
use holtmail_blob_fs::FsBlobStore;
use holtmail_core::{ArchiveId, DeviceProtocol, UserId};
use holtmail_device::DeviceAuth;
use holtmail_mailview::{MailActions, MailView};
use holtmail_serve_jmap::{Hub, JmapServer, Services, Settings, Wake};
use holtmail_store::Store;
use holtmail_submission::Submission;

use crate::config::Config;
use crate::mailview::{ChangeHub, HubListener};

/// The change hub as the JMAP server's wake-ups, with archive names from the store.
pub struct JmapHub {
    pub hub: Arc<ChangeHub>,
    pub store: Arc<dyn Store>,
}

#[async_trait]
impl Hub for JmapHub {
    fn listen(&self, archive: ArchiveId, user: UserId) -> Box<dyn Wake> {
        Box::new(self.hub.subscribe(archive, user))
    }

    async fn archive_name(&self, archive: ArchiveId) -> Option<String> {
        self.store
            .get_archive(archive)
            .await
            .ok()
            .flatten()
            .map(|a| a.name)
    }
}

#[async_trait]
impl Wake for HubListener {
    async fn changed(&mut self) -> Option<u64> {
        HubListener::changed(self).await
    }
}

/// Whether `[serve.jmap]` mounts the routes.
pub fn enabled(config: &Config) -> bool {
    config.serve.jmap.as_ref().is_some_and(|s| s.enabled)
}

/// The JMAP row for Settings → Devices: the host and port of an https `public_url`. Nothing for
/// plain http, which only loopback clients may use, or when JMAP is off.
pub fn endpoints(config: &Config) -> Vec<holtmail_api::ServeEndpoint> {
    if !enabled(config) {
        return Vec::new();
    }
    let Some(url) = config
        .public_url
        .as_deref()
        .and_then(|u| url::Url::parse(u).ok())
        .filter(|u| u.scheme() == "https")
    else {
        return Vec::new();
    };
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        return Vec::new();
    };
    vec![holtmail_api::ServeEndpoint {
        protocol: DeviceProtocol::Jmap,
        host: Some(host.to_owned()),
        port,
        implicit_tls: true,
        url: Some(format!(
            "{}/.well-known/jmap",
            url.as_str().trim_end_matches('/')
        )),
    }]
}

/// What one `router` needs besides the config.
pub struct Parts {
    pub view: Arc<dyn MailView>,
    pub actions: Arc<dyn MailActions>,
    pub submission: Arc<dyn Submission>,
    pub devices: Arc<dyn DeviceAuth>,
    /// Where uploads are kept: a blob store of their own, emptied at start.
    pub uploads_dir: PathBuf,
    pub store: Arc<dyn Store>,
    pub hub: Arc<ChangeHub>,
    pub instance_token: String,
}

/// The `/jmap` and `/.well-known/jmap` routes, or `None` when `[serve.jmap]` is off. Uploads
/// left from an earlier run are deleted: nothing refers to them any more.
pub async fn router(config: &Config, parts: Parts) -> anyhow::Result<Option<Router>> {
    if !enabled(config) {
        return Ok(None);
    }
    match tokio::fs::remove_dir_all(&parts.uploads_dir).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("emptying the JMAP upload directory"),
    }
    let blobs: Arc<dyn BlobStore> = Arc::new(FsBlobStore::open(&parts.uploads_dir).await?);
    let mut settings = Settings::new(config.base_url());
    settings.instance_token = Some(parts.instance_token);
    settings.trusted_proxies = config.serve.validate()?;
    let server = JmapServer::new(
        settings,
        Services {
            view: parts.view,
            actions: parts.actions,
            submission: parts.submission,
            devices: parts.devices,
            blobs,
            hub: Arc::new(JmapHub {
                hub: parts.hub,
                store: parts.store,
            }),
        },
    );
    tracing::info!("JMAP at /jmap");
    Ok(Some(server.router()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_name_the_https_address_only() {
        let off: Config = toml::from_str("public_url = \"https://mail.example.com/\"").unwrap();
        assert!(endpoints(&off).is_empty());
        let disabled: Config = toml::from_str(
            "public_url = \"https://mail.example.com/\"\n[serve.jmap]\nenabled = false",
        )
        .unwrap();
        assert!(endpoints(&disabled).is_empty());
        let plain: Config =
            toml::from_str("public_url = \"http://127.0.0.1:8080\"\n[serve.jmap]").unwrap();
        assert!(endpoints(&plain).is_empty());
        let on: Config =
            toml::from_str("public_url = \"https://mail.example.com:8443/\"\n[serve.jmap]\n")
                .unwrap();
        let rows: Vec<_> = endpoints(&on)
            .into_iter()
            .map(|r| (r.protocol, r.host, r.port, r.implicit_tls, r.url))
            .collect();
        assert_eq!(
            rows,
            [(
                DeviceProtocol::Jmap,
                Some("mail.example.com".to_owned()),
                8443,
                true,
                Some("https://mail.example.com:8443/.well-known/jmap".to_owned()),
            )]
        );
    }

    #[test]
    fn jmap_needs_public_url() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("holtmail.toml");
        std::fs::write(
            &path,
            format!("data_dir = {:?}\n[serve.jmap]\n", dir.path().join("data")),
        )
        .unwrap();
        let error = Config::load(Some(&path)).unwrap_err().to_string();
        assert!(error.contains("public_url"), "{error}");
    }
}
