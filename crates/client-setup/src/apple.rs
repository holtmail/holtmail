use uuid::{Builder, Uuid};

use crate::plist::{Value, document};
use crate::{Account, Server, SetupError};

/// What an Apple configuration profile sets up for one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The instance's stable id; with `device` it fixes every `PayloadUUID`.
    pub instance: String,
    /// The device the profile is for.
    pub device: String,
    /// The user the device belongs to.
    pub account: Account,
    /// The device's app password, written into every account of the profile.
    pub app_password: String,
    /// An IMAP account with SMTP submission.
    pub mail: bool,
    /// A CalDAV account.
    pub calendars: bool,
    /// A CardDAV account named "People".
    pub contacts: bool,
}

/// Renders an unsigned Apple `.mobileconfig` profile (XML property list) for iOS and macOS.
///
/// Guarantees:
/// - The mail account uses implicit TLS when the server offers it and STARTTLS otherwise (Apple's
///   `UseSSL` covers both); the outgoing password is the incoming one. DAV accounts point at [`Server::dav_url`].
/// - Every `PayloadUUID` and `PayloadIdentifier` derives from `instance`, `device` and the
///   payload's kind only, so a profile rendered again for the same device replaces the installed
///   one instead of adding a second, and two devices never share one.
/// - Every string is XML-escaped; characters XML 1.0 cannot carry are dropped.
/// - The output holds the app password: it is a secret and must be handed out like one.
///
/// Fails with [`SetupError::Empty`] when nothing is asked for, [`SetupError::NotOffered`] when
/// mail is asked for without both IMAP and submission, or DAV without DAV, and
/// [`SetupError::Invalid`] for malformed input.
pub fn mobileconfig(server: &Server, profile: &Profile) -> Result<String, SetupError> {
    server.validate()?;
    profile.account.validate()?;
    if profile.instance.is_empty() {
        return Err(SetupError::Invalid("instance"));
    }
    if profile.device.is_empty() {
        return Err(SetupError::Invalid("device"));
    }
    if profile.app_password.is_empty() || profile.app_password.chars().any(char::is_control) {
        return Err(SetupError::Invalid("app_password"));
    }
    if !(profile.mail || profile.calendars || profile.contacts) {
        return Err(SetupError::Empty);
    }

    let root_uuid = payload_uuid(profile, "profile");
    let root_id = format!(
        "{}.holtmail.{}",
        reversed(&server.web_host),
        root_uuid.to_lowercase()
    );
    let mut content = Vec::new();
    if profile.mail {
        content.push(mail_payload(server, profile, &root_id)?);
    }
    if profile.calendars {
        content.push(dav_payload(server, profile, &root_id, Dav::Cal)?);
    }
    if profile.contacts {
        content.push(dav_payload(server, profile, &root_id, Dav::Card)?);
    }

    let login = &profile.account.login;
    let root = Value::Dict(vec![
        ("PayloadContent", Value::Array(content)),
        (
            "PayloadDescription",
            Value::string(format!("Accounts from {} for {login}.", server.name)),
        ),
        (
            "PayloadDisplayName",
            Value::string(format!("{} ({login})", server.name)),
        ),
        ("PayloadIdentifier", Value::string(root_id)),
        ("PayloadOrganization", Value::string(&server.name)),
        ("PayloadRemovalDisallowed", Value::Bool(false)),
        ("PayloadType", Value::string("Configuration")),
        ("PayloadUUID", Value::string(root_uuid)),
        ("PayloadVersion", Value::Integer(1)),
    ]);
    Ok(document(&root))
}

fn mail_payload(server: &Server, profile: &Profile, root_id: &str) -> Result<Value, SetupError> {
    let (imap_port, _) = server
        .imap
        .preferred()
        .ok_or(SetupError::NotOffered("imap"))?;
    let (smtp_port, _) = server
        .submission
        .preferred()
        .ok_or(SetupError::NotOffered("submission"))?;
    let account = &profile.account;
    Ok(Value::Dict(vec![
        ("EmailAccountDescription", Value::string(&server.name)),
        ("EmailAccountName", Value::string(&account.display_name)),
        ("EmailAccountType", Value::string("EmailTypeIMAP")),
        ("EmailAddress", Value::string(&account.login)),
        (
            "IncomingMailServerAuthentication",
            Value::string("EmailAuthPassword"),
        ),
        (
            "IncomingMailServerHostName",
            Value::string(&server.mail_host),
        ),
        (
            "IncomingMailServerPortNumber",
            Value::Integer(imap_port.into()),
        ),
        ("IncomingMailServerUseSSL", Value::Bool(true)),
        ("IncomingMailServerUsername", Value::string(&account.login)),
        ("IncomingPassword", Value::string(&profile.app_password)),
        (
            "OutgoingMailServerAuthentication",
            Value::string("EmailAuthPassword"),
        ),
        (
            "OutgoingMailServerHostName",
            Value::string(&server.mail_host),
        ),
        (
            "OutgoingMailServerPortNumber",
            Value::Integer(smtp_port.into()),
        ),
        ("OutgoingMailServerUseSSL", Value::Bool(true)),
        ("OutgoingMailServerUsername", Value::string(&account.login)),
        ("OutgoingPasswordSameAsIncomingPassword", Value::Bool(true)),
        (
            "PayloadDescription",
            Value::string(format!("Mail from {}.", server.name)),
        ),
        (
            "PayloadDisplayName",
            Value::string(format!("{} Mail", server.name)),
        ),
        (
            "PayloadIdentifier",
            Value::string(format!("{root_id}.mail")),
        ),
        ("PayloadType", Value::string("com.apple.mail.managed")),
        ("PayloadUUID", Value::string(payload_uuid(profile, "mail"))),
        ("PayloadVersion", Value::Integer(1)),
    ]))
}

const CALDAV_KEYS: [&str; 7] = [
    "CalDAVAccountDescription",
    "CalDAVHostName",
    "CalDAVPassword",
    "CalDAVPort",
    "CalDAVPrincipalURL",
    "CalDAVUseSSL",
    "CalDAVUsername",
];

const CARDDAV_KEYS: [&str; 7] = [
    "CardDAVAccountDescription",
    "CardDAVHostName",
    "CardDAVPassword",
    "CardDAVPort",
    "CardDAVPrincipalURL",
    "CardDAVUseSSL",
    "CardDAVUsername",
];

#[derive(Clone, Copy)]
enum Dav {
    Cal,
    Card,
}

fn dav_payload(
    server: &Server,
    profile: &Profile,
    root_id: &str,
    kind: Dav,
) -> Result<Value, SetupError> {
    if !server.dav {
        return Err(SetupError::NotOffered("dav"));
    }
    let (keys, noun, description, display, kind_id, payload_type) = match kind {
        Dav::Cal => (
            CALDAV_KEYS,
            "Calendars",
            server.name.clone(),
            format!("{} Calendars", server.name),
            "caldav",
            "com.apple.caldav.account",
        ),
        Dav::Card => (
            CARDDAV_KEYS,
            "People",
            "People".to_owned(),
            format!("{} People", server.name),
            "carddav",
            "com.apple.carddav.account",
        ),
    };
    let [
        k_description,
        k_host,
        k_password,
        k_port,
        k_principal,
        k_ssl,
        k_username,
    ] = keys;
    Ok(Value::Dict(vec![
        (k_description, Value::string(description)),
        (k_host, Value::string(&server.web_host)),
        (k_password, Value::string(&profile.app_password)),
        (k_port, Value::Integer(server.web_port.into())),
        (k_principal, Value::string(server.dav_url())),
        (k_ssl, Value::Bool(server.web_tls)),
        (k_username, Value::string(&profile.account.login)),
        (
            "PayloadDescription",
            Value::string(format!("{noun} from {}.", server.name)),
        ),
        ("PayloadDisplayName", Value::string(display)),
        (
            "PayloadIdentifier",
            Value::string(format!("{root_id}.{kind_id}")),
        ),
        ("PayloadType", Value::string(payload_type)),
        ("PayloadUUID", Value::string(payload_uuid(profile, kind_id))),
        ("PayloadVersion", Value::Integer(1)),
    ]))
}

fn payload_uuid(profile: &Profile, kind: &str) -> String {
    let mut hasher = blake3::Hasher::new_derive_key("holtmail client-setup 2026-11 PayloadUUID");
    for part in [profile.instance.as_str(), profile.device.as_str(), kind] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    let uuid: Uuid = Builder::from_custom_bytes(bytes).into_uuid();
    uuid.hyphenated().to_string().to_uppercase()
}

fn reversed(host: &str) -> String {
    host.to_ascii_lowercase()
        .rsplit('.')
        .collect::<Vec<_>>()
        .join(".")
}
