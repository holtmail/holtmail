//! A minimal DKIM signer (RFC 6376, `rsa-sha256`, `relaxed/relaxed`) for invented messages, so a
//! suite can send mail that a real verifier (OpenDKIM in `sl-mx`) checks against a key the stack
//! publishes in its own DNS.

use std::path::Path;

use aws_lc_rs::digest::{SHA256, digest};
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// The domain whose key the `aliases` profile publishes for test-signed mail.
pub const NEWS_DOMAIN: &str = "news.example.com";
/// The selector of that key: `it2026._domainkey.news.example.com`.
pub const NEWS_SELECTOR: &str = "it2026";

/// The private key of [`NEWS_DOMAIN`], written by the SimpleLogin seed of the `aliases` profile.
pub fn news_key() -> Signer {
    Signer::from_pem_file(
        &crate::state_dir().join("sl").join("news-dkim.key"),
        NEWS_DOMAIN,
        NEWS_SELECTOR,
    )
}

/// Signs messages for one domain and selector.
pub struct Signer {
    key: RsaKeyPair,
    domain: String,
    selector: String,
}

impl Signer {
    /// Reads a PKCS#1 PEM (`BEGIN RSA PRIVATE KEY`) key.
    pub fn from_pem_file(path: &Path, domain: &str, selector: &str) -> Self {
        let pem = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{}: {e}; run with IT_PROFILES=aliases", path.display()));
        let body: String = pem
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect();
        let der = STANDARD.decode(body.trim()).expect("a base64 PEM body");
        Self {
            key: RsaKeyPair::from_der(&der).expect("a PKCS#1 RSA key"),
            domain: domain.to_owned(),
            selector: selector.to_owned(),
        }
    }

    /// The message made of `headers` (name, value) and `body`, with CRLF line ends and a
    /// `DKIM-Signature` on top that signs the fields named in `signed` (each once, the last
    /// field of that name, as verifiers pick them).
    pub fn sign(&self, headers: &[(&str, &str)], body: &str, signed: &[&str]) -> Vec<u8> {
        let body = body.replace("\r\n", "\n").replace('\n', "\r\n");
        let body_hash = STANDARD.encode(digest(&SHA256, &relaxed_body(&body)).as_ref());
        let mut input = String::new();
        for name in signed {
            let (field, value) = headers
                .iter()
                .rev()
                .find(|(field, _)| field.eq_ignore_ascii_case(name))
                .unwrap_or_else(|| panic!("no {name} field to sign"));
            input.push_str(&relaxed_header(field, value));
        }
        let unsigned = format!(
            "v=1; a=rsa-sha256; c=relaxed/relaxed; d={}; s={}; h={}; bh={body_hash}; b=",
            self.domain,
            self.selector,
            signed.join(":")
        );
        input.push_str(relaxed_header("DKIM-Signature", &unsigned).trim_end_matches("\r\n"));
        let mut signature = vec![0; self.key.public_modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                input.as_bytes(),
                &mut signature,
            )
            .expect("signed");
        let mut out = format!(
            "DKIM-Signature: {unsigned}{}\r\n",
            STANDARD.encode(signature)
        );
        for (field, value) in headers {
            out.push_str(&format!("{field}: {value}\r\n"));
        }
        out.push_str("\r\n");
        out.push_str(&body);
        out.into_bytes()
    }
}

fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c == ' ' || c == '\t' {
            space = true;
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    out
}

fn relaxed_header(name: &str, value: &str) -> String {
    let unfolded = value.replace("\r\n", "");
    format!(
        "{}:{}\r\n",
        name.trim().to_ascii_lowercase(),
        collapse(&unfolded).trim()
    )
}

fn relaxed_body(body: &str) -> Vec<u8> {
    let mut lines: Vec<String> = body
        .split("\r\n")
        .map(|line| collapse(line).trim_end().to_owned())
        .collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let mut out = String::new();
    for line in lines {
        out.push_str(&line);
        out.push_str("\r\n");
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_relaxed() {
        assert_eq!(
            relaxed_header("Subject ", "  A \t  long\r\n\tsubject  "),
            "subject:A long subject\r\n"
        );
        assert_eq!(
            relaxed_body("a  b \t\r\n\r\nc\r\n\r\n\r\n"),
            b"a b\r\n\r\nc\r\n"
        );
        assert_eq!(relaxed_body("\r\n\r\n"), b"");
    }
}
