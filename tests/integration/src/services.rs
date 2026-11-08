//! Where the stack's servers listen on this machine, and how to control their containers.

use std::process::Command;
use std::time::Duration;

/// Dovecot: plain IMAP with STARTTLS.
pub const DOVECOT_IMAP: u16 = 41143;
/// Dovecot: IMAP over TLS, with a certificate from the stack's CA for `localhost`.
pub const DOVECOT_IMAPS: u16 = 41993;
/// The password of every Dovecot user; any user name signs in with it, and gets a fresh
/// mailbox on first use.
pub const DOVECOT_PASSWORD: &str = "it-imap-password";
/// GreenMail: plain IMAP without CONDSTORE. Authentication is off: any user name and password
/// sign in, and a user is created on first use.
pub const GREENMAIL_IMAP: u16 = 42143;
/// GreenMail: IMAP over TLS with GreenMail's own self-signed certificate.
pub const GREENMAIL_IMAPS: u16 = 42993;
/// GreenMail: plain SMTP without sign-in; it takes mail for any address and files it for the
/// user of that address.
pub const GREENMAIL_SMTP: u16 = 42587;

/// `127.0.0.1:<port>`.
pub fn local(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

/// An HTTP client that trusts the stack's CA, for seeding DAV, JMAP and S3 servers.
pub fn http_client() -> reqwest::Client {
    let pem = std::fs::read(crate::ca_file()).expect("the stack's CA; run `mise run it`");
    let roots = reqwest::Certificate::from_pem_bundle(&pem).expect("a PEM CA");
    reqwest::Client::builder()
        .tls_certs_merge(roots)
        .timeout(Duration::from_secs(60))
        .build()
        .expect("client")
}

/// Runs `docker compose` with `args` on the stack, such as `["pause", "dovecot"]`, and
/// returns its output; fails the test when it fails.
pub fn compose(args: &[&str]) -> String {
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("compose.yaml");
    let output = Command::new("docker")
        .arg("compose")
        .arg("-f")
        .arg(&file)
        .args(args)
        .output()
        .expect("docker runs");
    assert!(
        output.status.success(),
        "docker compose {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// An IMAP server of the stack, reached over plain IMAP by the seeder and by Holtmail.
#[derive(Debug, Clone, Copy)]
pub struct ImapServer {
    /// The compose service.
    pub service: &'static str,
    /// Plain IMAP port on this machine.
    pub port: u16,
    /// The password every user signs in with.
    pub password: &'static str,
    /// The server's hierarchy delimiter.
    pub delimiter: char,
}

/// Dovecot, with CONDSTORE and special-use folders.
pub const DOVECOT: ImapServer = ImapServer {
    service: "dovecot",
    port: DOVECOT_IMAP,
    password: DOVECOT_PASSWORD,
    delimiter: '/',
};

/// GreenMail, without CONDSTORE or special-use folders.
pub const GREENMAIL: ImapServer = ImapServer {
    service: "greenmail",
    port: GREENMAIL_IMAP,
    password: "it-greenmail-password",
    delimiter: '.',
};

/// Stalwart over plain IMAP; users are created through its management API by [`ImapServer::seeder`].
pub const STALWART: ImapServer = ImapServer {
    service: "stalwart",
    port: crate::stalwart::STALWART_IMAP,
    password: crate::stalwart::STALWART_PASSWORD,
    delimiter: '/',
};

impl ImapServer {
    /// A fresh user on this server.
    pub fn user(&self, prefix: &str) -> String {
        format!("{}@example.com", crate::unique(prefix))
    }

    /// Signs `user` in for seeding; on Stalwart, creates the user first.
    pub async fn seeder(&self, user: &str) -> crate::imap::Seeder {
        if self.service == STALWART.service {
            crate::stalwart::create_user(user).await;
        }
        crate::imap::Seeder::login(&local(self.port), user, self.password).await
    }

    /// The server's name for the folder Holtmail shows as `path` (levels split by `/`).
    pub fn folder(&self, path: &str) -> String {
        path.split('/')
            .map(crate::imap::utf7)
            .collect::<Vec<_>>()
            .join(&self.delimiter.to_string())
    }

    /// The JSON to add `user` as an IMAP account over plain IMAP.
    pub fn account(&self, user: &str) -> serde_json::Value {
        serde_json::json!({
            "host": "127.0.0.1",
            "port": self.port,
            "security": "plain",
            "username": user,
            "password": self.password,
        })
    }
}

/// Makes Dovecot refuse `user` from now on, as a locked account, and waits until it does:
/// Dovecot reads the list again when it changes, which the container sees after a moment.
pub async fn lock_dovecot_user(user: &str) {
    use std::io::Write;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(crate::state_dir().join("dovecot-locked-users"))
        .expect("the locked users list");
    writeln!(file, "{user}").expect("written");
    drop(file);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let stream = tokio::net::TcpStream::connect(local(DOVECOT_IMAP))
            .await
            .expect("Dovecot answers");
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        stream.read_line(&mut line).await.expect("greeting");
        stream
            .get_mut()
            .write_all(
                format!(
                    "a LOGIN {} {}\r\n",
                    crate::imap::quote(user),
                    crate::imap::quote(DOVECOT_PASSWORD)
                )
                .as_bytes(),
            )
            .await
            .expect("write");
        let answer = loop {
            line.clear();
            stream.read_line(&mut line).await.expect("answer");
            if line.starts_with("a ") || line.is_empty() {
                break line.clone();
            }
        };
        if !answer.starts_with("a OK") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Dovecot still signs {user} in"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
