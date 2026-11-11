//! Client configuration for a Holtmail account, rendered from typed inputs: an Apple
//! `.mobileconfig` profile, Thunderbird autoconfig, the DAVx5 hand-off link and the DNS records
//! for an operator's own mail domain.
//!
//! Pure functions: no I/O, no clock, no randomness. The same input always renders the same bytes,
//! so a profile downloaded twice for one device is the same profile.

mod apple;
mod davx5;
mod dns;
mod plist;
mod thunderbird;
mod xml;

#[cfg(test)]
mod tests;

pub use apple::{Profile, mobileconfig};
pub use davx5::davx5_link;
pub use dns::{DNS_TTL, DnsRecord, RecordData, dns_records, zone_text};
pub use thunderbird::autoconfig;

/// The path every DAV client starts discovery from; a `PROPFIND` there names the principal.
pub const DAV_PATH: &str = "/dav/";

/// The ports a mail protocol listens on, as clients reach them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ports {
    /// The implicit TLS port (993 for IMAP, 465 for submission), if offered.
    pub tls: Option<u16>,
    /// The STARTTLS port (143 for IMAP, 587 for submission), if offered.
    pub starttls: Option<u16>,
}

impl Ports {
    /// Whether any port is offered.
    pub fn offered(&self) -> bool {
        self.tls.is_some() || self.starttls.is_some()
    }

    /// The port a client should use: implicit TLS when offered (RFC 8314), otherwise STARTTLS.
    pub fn preferred(&self) -> Option<(u16, Security)> {
        self.tls
            .map(|port| (port, Security::Tls))
            .or(self.starttls.map(|port| (port, Security::StartTls)))
    }
}

/// How a connection is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// TLS from the first byte.
    Tls,
    /// Plain text upgraded with `STARTTLS` before sign-in.
    StartTls,
}

/// What a Holtmail instance offers to clients and under which names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    /// The instance's name as people see it, such as "Holtmail".
    pub name: String,
    /// The host IMAP and submission clients connect to.
    pub mail_host: String,
    /// The host of the web address, which also serves JMAP and DAV.
    pub web_host: String,
    /// The port of the web address.
    pub web_port: u16,
    /// Whether the web address is `https`.
    pub web_tls: bool,
    /// IMAP ports.
    pub imap: Ports,
    /// SMTP submission ports.
    pub submission: Ports,
    /// Whether JMAP is served at `/jmap` with `/.well-known/jmap`.
    pub jmap: bool,
    /// Whether CalDAV and CardDAV are served at [`DAV_PATH`].
    pub dav: bool,
}

impl Server {
    /// The DAV discovery address, such as `https://mail.example.com/dav/`.
    pub fn dav_url(&self) -> String {
        format!("{}{DAV_PATH}", self.web_origin())
    }

    /// The JMAP session discovery address, such as `https://mail.example.com/.well-known/jmap`.
    pub fn jmap_url(&self) -> String {
        format!("{}/.well-known/jmap", self.web_origin())
    }

    fn web_origin(&self) -> String {
        let (scheme, default) = if self.web_tls {
            ("https", 443)
        } else {
            ("http", 80)
        };
        if self.web_port == default {
            format!("{scheme}://{}", self.web_host)
        } else {
            format!("{scheme}://{}:{}", self.web_host, self.web_port)
        }
    }

    fn validate(&self) -> Result<(), SetupError> {
        check_host(&self.mail_host, "mail_host")?;
        check_host(&self.web_host, "web_host")?;
        if self.web_port == 0 {
            return Err(SetupError::Invalid("web_port"));
        }
        for port in [
            self.imap.tls,
            self.imap.starttls,
            self.submission.tls,
            self.submission.starttls,
        ] {
            if port == Some(0) {
                return Err(SetupError::Invalid("port"));
            }
        }
        check_text(&self.name, "name")
    }
}

/// The user a configuration is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// The sign-in login: the user's email address.
    pub login: String,
    /// The name shown on sent mail and in the client's account list.
    pub display_name: String,
}

impl Account {
    fn validate(&self) -> Result<(), SetupError> {
        let Some((local, domain)) = self.login.rsplit_once('@') else {
            return Err(SetupError::Invalid("login"));
        };
        if local.is_empty()
            || self
                .login
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(SetupError::Invalid("login"));
        }
        check_host(domain, "login")?;
        check_text(&self.display_name, "display_name")
    }
}

/// Why a configuration could not be rendered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetupError {
    /// The named input is empty or malformed: a host that is not a DNS name, a login that is not
    /// an address, port 0, or text holding control characters.
    #[error("invalid {0}")]
    Invalid(&'static str),
    /// The configuration needs a service this server does not offer.
    #[error("{0} is not offered by this server")]
    NotOffered(&'static str),
    /// The request asked for nothing to configure.
    #[error("nothing to configure")]
    Empty,
}

fn check_host(host: &str, field: &'static str) -> Result<(), SetupError> {
    let valid = !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(SetupError::Invalid(field))
    }
}

fn check_text(text: &str, field: &'static str) -> Result<(), SetupError> {
    if text.trim().is_empty() || text.chars().any(char::is_control) {
        Err(SetupError::Invalid(field))
    } else {
        Ok(())
    }
}
