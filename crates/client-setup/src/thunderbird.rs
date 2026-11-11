use crate::xml::escape;
use crate::{Ports, Security, Server, SetupError, check_host};

/// Renders Thunderbird autoconfig (`config-v1.1.xml`) for addresses at `domain`.
///
/// Guarantees:
/// - Lists IMAP and SMTP submission, implicit TLS before STARTTLS for each, with the address as
///   the login (`%EMAILADDRESS%`) and a plain password, which is the app password.
/// - Adds the CalDAV and CardDAV address when DAV is offered; JMAP has no autoconfig entry.
/// - Holds no secret and nothing about one user, so it can be served to anyone who asks.
///
/// Fails with [`SetupError::NotOffered`] when the server offers no IMAP, since the format needs an
/// incoming server, and [`SetupError::Invalid`] for malformed input.
pub fn autoconfig(server: &Server, domain: &str) -> Result<String, SetupError> {
    server.validate()?;
    check_host(domain, "domain")?;
    if !server.imap.offered() {
        return Err(SetupError::NotOffered("imap"));
    }
    let domain = escape(&domain.to_ascii_lowercase());
    let name = escape(&server.name);
    let host = escape(&server.mail_host);
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <clientConfig version=\"1.1\">\n\
         \t<emailProvider id=\"{domain}\">\n\
         \t\t<domain>{domain}</domain>\n\
         \t\t<displayName>{name}</displayName>\n\
         \t\t<displayShortName>{name}</displayShortName>\n"
    );
    for (port, security) in each(server.imap) {
        server_entry(&mut out, "incomingServer", "imap", &host, port, security);
    }
    for (port, security) in each(server.submission) {
        server_entry(&mut out, "outgoingServer", "smtp", &host, port, security);
    }
    out.push_str("\t</emailProvider>\n");
    if server.dav {
        let url = escape(&server.dav_url());
        for (element, kind) in [("addressBook", "carddav"), ("calendar", "caldav")] {
            out.push_str(&format!(
                "\t<{element} type=\"{kind}\">\n\
                 \t\t<username>%EMAILADDRESS%</username>\n\
                 \t\t<authentication>http-basic</authentication>\n\
                 \t\t<serverURL>{url}</serverURL>\n\
                 \t</{element}>\n"
            ));
        }
    }
    out.push_str("</clientConfig>\n");
    Ok(out)
}

fn each(ports: Ports) -> impl Iterator<Item = (u16, Security)> {
    ports
        .tls
        .map(|port| (port, Security::Tls))
        .into_iter()
        .chain(ports.starttls.map(|port| (port, Security::StartTls)))
}

fn server_entry(
    out: &mut String,
    element: &str,
    kind: &str,
    host: &str,
    port: u16,
    security: Security,
) {
    let socket = match security {
        Security::Tls => "SSL",
        Security::StartTls => "STARTTLS",
    };
    out.push_str(&format!(
        "\t\t<{element} type=\"{kind}\">\n\
         \t\t\t<hostname>{host}</hostname>\n\
         \t\t\t<port>{port}</port>\n\
         \t\t\t<socketType>{socket}</socketType>\n\
         \t\t\t<authentication>password-cleartext</authentication>\n\
         \t\t\t<username>%EMAILADDRESS%</username>\n\
         \t\t</{element}>\n"
    ));
}
