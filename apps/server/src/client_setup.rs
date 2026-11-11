use anyhow::Context;
use holtmail_client_setup::{Ports, Server, dns_records, zone_text};

use crate::config::Config;

fn ports(tls: &[std::net::SocketAddr], starttls: &[std::net::SocketAddr]) -> Ports {
    Ports {
        tls: tls.iter().map(|a| a.port()).min(),
        starttls: starttls.iter().map(|a| a.port()).min(),
    }
}

/// What clients are told about this instance, from `public_url` and the `[serve]` listeners;
/// `None` without a `public_url`.
pub fn server(config: &Config) -> Option<Server> {
    let url = url::Url::parse(config.public_url.as_deref()?).ok()?;
    let web_host = url.host_str()?.to_owned();
    let web_port = url.port_or_known_default()?;
    let imap = config.serve.imap.as_ref();
    let submit = config.serve.submit.as_ref();
    let mail_host = crate::submit::configured_host(
        imap.and_then(|s| s.hostname.as_deref())
            .or(submit.and_then(|s| s.hostname.as_deref())),
        config,
    )?;
    Some(Server {
        name: "Holtmail".into(),
        mail_host,
        web_host,
        web_port,
        web_tls: url.scheme() == "https",
        imap: imap.map_or_else(Ports::default, |s| ports(&s.listen_tls, &s.listen_starttls)),
        submission: submit
            .map_or_else(Ports::default, |s| ports(&s.listen_tls, &s.listen_starttls)),
        jmap: crate::jmap::enabled(config),
        dav: crate::serve_dav::enabled(config),
    })
}

/// The API's client setup: the server, the instance id and the operator's own mail domains.
pub fn api(config: &Config, instance: &str) -> Option<holtmail_api::ClientSetup> {
    Some(holtmail_api::ClientSetup {
        server: server(config)?,
        instance: instance.to_owned(),
        mail_domains: config.serve.mail_domains(),
    })
}

/// The SRV and TXT records for every `[serve] mail_domains` entry, as zone-file text.
pub fn zone(config: &Config) -> anyhow::Result<String> {
    let server = server(config).context("the records need public_url")?;
    let domains = config.serve.mail_domains();
    anyhow::ensure!(
        !domains.is_empty(),
        "[serve] mail_domains names no domain; records only help in a mail domain you control"
    );
    let mut out = String::new();
    for domain in domains {
        let records =
            dns_records(&server, &domain).with_context(|| format!("records for {domain}"))?;
        out.push_str(&format!("; {domain}\n{}", zone_text(&records)));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}
