use crate::{DAV_PATH, Server, SetupError};

/// The link that opens DAVx5's login with the server and login filled in, such as
/// `davx5://alice%40example.com@mail.example.com/dav/`.
///
/// Guarantees:
/// - Uses the `davx5://` scheme, which DAVx5 opens as `https`; the login is percent-encoded into
///   the user part and the path is [`DAV_PATH`], where discovery starts.
/// - Never carries the app password: links end up in browser history, logs and screenshots, so
///   the person types or pastes the password into DAVx5.
///
/// Fails with [`SetupError::NotOffered`] when DAV is not offered or the web address is not
/// `https`, and [`SetupError::Invalid`] for a malformed login.
pub fn davx5_link(server: &Server, login: &str) -> Result<String, SetupError> {
    server.validate()?;
    if !server.dav {
        return Err(SetupError::NotOffered("dav"));
    }
    if !server.web_tls {
        return Err(SetupError::NotOffered("https"));
    }
    if login.is_empty() || login.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(SetupError::Invalid("login"));
    }
    let port = if server.web_port == 443 {
        String::new()
    } else {
        format!(":{}", server.web_port)
    };
    Ok(format!(
        "davx5://{}@{}{port}{DAV_PATH}",
        encode(login),
        server.web_host
    ))
}

fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
