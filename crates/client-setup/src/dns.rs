use std::fmt;

use crate::{DAV_PATH, Server, SetupError, check_host};

/// The time to live given to every record, in seconds.
pub const DNS_TTL: u32 = 3600;

/// One DNS record for an operator's mail domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsRecord {
    /// The owner name, fully qualified with the trailing dot.
    pub name: String,
    /// The time to live in seconds.
    pub ttl: u32,
    /// The record's type and data.
    pub data: RecordData,
}

/// The type and data of a [`DnsRecord`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordData {
    /// An SRV record (RFC 2782). A target of `.` with port 0 says the service is not offered.
    Srv {
        /// Lower is tried first.
        priority: u16,
        /// Share among records of equal priority.
        weight: u16,
        /// The port.
        port: u16,
        /// The host, fully qualified with the trailing dot, or `.`.
        target: String,
    },
    /// A TXT record with one string.
    Txt(String),
}

impl fmt::Display for DnsRecord {
    /// One zone-file line: `<name> <ttl> IN <type> <data>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.data {
            RecordData::Srv {
                priority,
                weight,
                port,
                target,
            } => write!(
                f,
                "{} {} IN SRV {priority} {weight} {port} {target}",
                self.name, self.ttl
            ),
            RecordData::Txt(text) => write!(f, "{} {} IN TXT \"{text}\"", self.name, self.ttl),
        }
    }
}

/// The DNS records that let clients find this server from addresses at `domain`.
///
/// Guarantees, in this order:
/// - RFC 6186 and RFC 8314: `_imaps`, `_imap`, `_submissions` and `_submission` under `_tcp`,
///   each pointing at the mail host or, when that port is not offered, at `.` (not available);
///   implicit TLS gets priority 0 and STARTTLS 10, so RFC 6186 clients that weigh both prefer
///   implicit TLS (RFC 8314); `_pop3s` and `_pop3` always say not available.
/// - RFC 8620: `_jmap._tcp` at the web host when JMAP is offered over `https`.
/// - RFC 6764: `_caldavs` and `_carddavs` (or `_caldav` and `_carddav` when the web address is not
///   `https`) at the web host with a `path=` TXT record for [`DAV_PATH`], when DAV is offered.
///
/// These only help when the operator controls `domain`'s zone. Fails with
/// [`SetupError::Invalid`] for a malformed domain or server.
pub fn dns_records(server: &Server, domain: &str) -> Result<Vec<DnsRecord>, SetupError> {
    server.validate()?;
    check_host(domain, "domain")?;
    let domain = domain.to_ascii_lowercase();
    let mail_host = format!("{}.", server.mail_host.to_ascii_lowercase());
    let web_host = format!("{}.", server.web_host.to_ascii_lowercase());
    let name = |service: &str| format!("_{service}._tcp.{domain}.");
    let srv = |service: &str, priority: u16, port: Option<u16>, target: &str| DnsRecord {
        name: name(service),
        ttl: DNS_TTL,
        data: match port {
            Some(port) => RecordData::Srv {
                priority,
                weight: 1,
                port,
                target: target.into(),
            },
            None => RecordData::Srv {
                priority: 0,
                weight: 0,
                port: 0,
                target: ".".into(),
            },
        },
    };

    let mut records = vec![
        srv("imaps", 0, server.imap.tls, &mail_host),
        srv("imap", 10, server.imap.starttls, &mail_host),
        srv("submissions", 0, server.submission.tls, &mail_host),
        srv("submission", 10, server.submission.starttls, &mail_host),
        srv("pop3s", 0, None, ""),
        srv("pop3", 0, None, ""),
    ];
    if server.jmap && server.web_tls {
        records.push(srv("jmap", 0, Some(server.web_port), &web_host));
    }
    if server.dav {
        let suffix = if server.web_tls { "s" } else { "" };
        for service in ["caldav", "carddav"] {
            let service = format!("{service}{suffix}");
            records.push(srv(&service, 0, Some(server.web_port), &web_host));
            records.push(DnsRecord {
                name: name(&service),
                ttl: DNS_TTL,
                data: RecordData::Txt(format!("path={DAV_PATH}")),
            });
        }
    }
    Ok(records)
}

/// The records as zone-file text, one per line, with the names padded to one column.
pub fn zone_text(records: &[DnsRecord]) -> String {
    let width = records.iter().map(|r| r.name.len()).max().unwrap_or(0);
    records
        .iter()
        .map(|record| {
            let line = record.to_string();
            let rest = &line[record.name.len()..];
            format!("{:width$}{rest}\n", record.name)
        })
        .collect()
}
