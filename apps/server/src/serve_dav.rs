//! Serving CalDAV and CardDAV to calendar and contacts apps: the `[serve.dav]` routes on the main
//! listener over the archive's collections and their held edits.

use std::sync::Arc;

use axum::Router;
use holtmail_api::SchedulingProbe;
use holtmail_core::{ArchiveId, DeviceProtocol};
use holtmail_device::DeviceAuth;
use holtmail_ingest::Ingest;
use holtmail_schedule::{Host, HostCaps, Scheduler};
use holtmail_serve_dav::{CalendarHosts, Dav, DavOptions};
use holtmail_store::{Collection, Store};

use crate::config::Config;

/// Whether `[serve.dav]` mounts the routes.
pub fn enabled(config: &Config) -> bool {
    config.serve.dav.as_ref().is_some_and(|s| s.enabled)
}

/// The CalDAV and CardDAV row for Settings → Devices: the host and port of an https
/// `public_url`. Nothing for plain http, which only loopback clients may use, or when DAV is off.
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
        protocol: DeviceProtocol::Dav,
        host: Some(host.to_owned()),
        port,
        implicit_tls: true,
        url: Some(format!("{}/dav/", url.as_str().trim_end_matches('/'))),
    }]
}

/// The DAV server, or `None` when `[serve.dav]` is off. `changed` is called with the archive
/// after every change a client records, so writeback and People wake at once.
pub fn server(
    config: &Config,
    store: Arc<dyn Store>,
    ingest: Ingest,
    devices: Arc<dyn DeviceAuth>,
    changed: Arc<dyn Fn(ArchiveId) + Send + Sync>,
    scheduling: Option<(Arc<dyn Scheduler>, Arc<dyn CalendarHosts>)>,
) -> anyhow::Result<Option<Dav>> {
    let Some(section) = config.serve.dav.as_ref().filter(|s| s.enabled) else {
        return Ok(None);
    };
    let mut options = DavOptions::new(config.base_url());
    options.edits = section.edits;
    options.trusted_proxies = config.serve.validate()?;
    options.switches = config.write.into();
    options.scheduling = section.scheduling;
    tracing::info!(
        edits = section.edits,
        scheduling = section.scheduling,
        "CalDAV and CardDAV at /dav"
    );
    let dav = Dav::new(store, ingest, devices, options).with_hook(changed);
    Ok(Some(match scheduling {
        Some((scheduler, hosts)) => dav.with_scheduler(scheduler, hosts),
        None => dav,
    }))
}

/// The scheduling host of a calendar, from the collection writers' probe; unknown until the
/// writers exist.
pub struct Hosts(pub Arc<crate::config::Services>);

#[async_trait::async_trait]
impl CalendarHosts for Hosts {
    async fn host(&self, collection: &Collection) -> Option<(Host, HostCaps)> {
        let writers = self.0.pim_writers.get()?;
        let known = match writers.scheduling(collection).await {
            SchedulingProbe::NoServer => return Some((Host::Mail, HostCaps::default())),
            SchedulingProbe::Unknown => return None,
            SchedulingProbe::Known(known) => known,
        };
        let host = match known.host {
            holtmail_pim_edit::Host::CalDav { auto_schedule } => Host::CalDav { auto_schedule },
            holtmail_pim_edit::Host::Graph => Host::Graph,
            holtmail_pim_edit::Host::Jmap { scheduling } => Host::Jmap { scheduling },
            holtmail_pim_edit::Host::Mail => Host::Mail,
        };
        Some((
            host,
            HostCaps {
                create: known.caps.create,
                update: known.caps.update,
                delete: known.caps.delete,
                respond: known.caps.respond,
            },
        ))
    }
}

/// Adds the DAV routes and the root `PROPFIND` to `app`, which must already carry every other
/// route and its fallback, so that `PROPFIND /` reaches DAV whatever serves `/`.
pub fn mount(app: Router, dav: Option<Dav>) -> Router {
    match dav {
        Some(dav) => app
            .merge(dav.router())
            .layer(axum::middleware::from_fn_with_state(
                dav,
                holtmail_serve_dav::root_propfind,
            )),
        None => app,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_name_the_https_address_only() {
        let off: Config = toml::from_str("public_url = \"https://dav.example.com/\"").unwrap();
        assert!(endpoints(&off).is_empty());
        let plain: Config =
            toml::from_str("public_url = \"http://127.0.0.1:8080\"\n[serve.dav]").unwrap();
        assert!(endpoints(&plain).is_empty());
        let on: Config =
            toml::from_str("public_url = \"https://dav.example.com/\"\n[serve.dav]\n").unwrap();
        let rows: Vec<_> = endpoints(&on)
            .into_iter()
            .map(|r| (r.protocol, r.host, r.port, r.url))
            .collect();
        assert_eq!(
            rows,
            [(
                DeviceProtocol::Dav,
                Some("dav.example.com".to_owned()),
                443,
                Some("https://dav.example.com/dav/".to_owned()),
            )]
        );
        let below: Config =
            toml::from_str("public_url = \"https://example.com/holtmail/\"\n[serve.dav]\n")
                .unwrap();
        assert_eq!(
            endpoints(&below)[0].url.as_deref(),
            Some("https://example.com/holtmail/dav/")
        );
    }

    #[test]
    fn reads_the_section() {
        let config: Config =
            toml::from_str("public_url = \"https://dav.example.com\"\n[serve.dav]\nedits = false")
                .unwrap();
        let section = config.serve.dav.as_ref().unwrap();
        assert!(section.enabled);
        assert!(!section.edits);
        assert!(section.scheduling, "on by default");
        assert!(toml::from_str::<Config>("[serve.dav]\nscheduling = 1").is_err());
        let off: Config = toml::from_str("[serve.dav]\nscheduling = false").unwrap();
        assert!(!off.serve.dav.unwrap().scheduling);
    }
}
