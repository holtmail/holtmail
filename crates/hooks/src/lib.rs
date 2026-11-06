//! What Holtmail tells webhooks: the events, the JSON body of each delivery, its signature and
//! when a failed delivery is tried again.
//!
//! Payloads carry ids, counts, states and hashes, never message content, subjects or
//! addresses, so a webhook receiver learns that something happened, not what the mail says.

use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use holtmail_core::{AccountId, ArchiveId, Timestamp, UserId};
use serde::Serialize;
use sha2::Sha256;

/// Header with the event name.
pub const EVENT_HEADER: &str = "Holtmail-Event";
/// Header with the delivery's id, the same on every attempt.
pub const DELIVERY_HEADER: &str = "Holtmail-Delivery";
/// Header with the signature, `t=<unix seconds>,v1=<hex HMAC-SHA256>`.
pub const SIGNATURE_HEADER: &str = "Holtmail-Signature";

/// Every event name a webhook can subscribe to, besides `webhook.test`, which is always sent.
pub const EVENTS: &[&str] = &[
    "task.finished",
    "account.needs_sign_in",
    "messages.deleted",
    "ledger.verified",
];

const MAX_URL: usize = 2_000;

/// Checks a webhook's address and events and returns them tidied: the address trimmed, the
/// events sorted and each once. The address is `http` or `https` with a host and without a user
/// name or password, at most 2 000 characters; events come from [`EVENTS`]. The reason is a
/// sentence for people.
pub fn check(url: &str, events: &[String]) -> Result<(String, Vec<String>), String> {
    let url = url.trim();
    let address = || "Enter an http:// or https:// address.".to_owned();
    if url.len() > MAX_URL {
        return Err("The address is too long.".into());
    }
    let parsed = url::Url::parse(url).map_err(|_| address())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host().is_none() {
        return Err(address());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(
            "Put no user name or password in the address; check the signature instead.".into(),
        );
    }
    let mut wanted: Vec<String> = events.iter().map(|e| e.trim().to_owned()).collect();
    wanted.sort();
    wanted.dedup();
    if let Some(unknown) = wanted.iter().find(|e| !EVENTS.contains(&e.as_str())) {
        return Err(format!("Unknown event {unknown}."));
    }
    Ok((url.to_owned(), wanted))
}

/// Something that happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Event {
    /// A background task ended: a sync, an import, a deletion, a verification.
    TaskFinished {
        /// The task.
        task_id: String,
        /// `sync`, `import`, `remote_images`, `contacts_calendars`, `ledger_verify` or
        /// `delete_messages`.
        kind: String,
        /// `done`, `failed` or `cancelled`.
        state: String,
        /// Its archive.
        archive_id: Option<ArchiveId>,
        /// Its account.
        account_id: Option<AccountId>,
        /// Steps done.
        done: u64,
        /// Steps in all, when known.
        total: Option<u64>,
    },
    /// An account's sign-in was refused and it waits for someone to sign in again.
    AccountNeedsSignIn {
        /// The account.
        account_id: AccountId,
        /// Its archive.
        archive_id: ArchiveId,
        /// `google` or `microsoft`.
        provider: String,
    },
    /// Messages were deleted on purpose.
    MessagesDeleted {
        /// The archive.
        archive_id: ArchiveId,
        /// How many.
        messages: u64,
        /// Who confirmed the deletion; `null` from the command line.
        by: Option<UserId>,
        /// The ledger's newest position after the deletion.
        ledger_seq: Option<u64>,
        /// Its hash.
        ledger_hash: Option<String>,
    },
    /// The ledger was verified.
    LedgerVerified {
        /// Nothing was wrong.
        ok: bool,
        /// Entries checked.
        entries: u64,
        /// Problems found.
        problems: u64,
        /// The newest position checked.
        ledger_seq: Option<u64>,
        /// Its hash; keep it to prove later that nothing up to it changed.
        ledger_hash: Option<String>,
    },
    /// Sent on request to check a webhook.
    Test {},
}

impl Event {
    /// The event's name, as in [`EVENTS`] or `webhook.test`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::TaskFinished { .. } => "task.finished",
            Self::AccountNeedsSignIn { .. } => "account.needs_sign_in",
            Self::MessagesDeleted { .. } => "messages.deleted",
            Self::LedgerVerified { .. } => "ledger.verified",
            Self::Test {} => "webhook.test",
        }
    }
}

#[derive(Serialize)]
struct Payload<'a> {
    id: &'a str,
    event: &'static str,
    at: i64,
    data: &'a Event,
}

/// The JSON body of a delivery: `{ "id", "event", "at" (Unix milliseconds), "data" }`.
pub fn payload(id: &str, at: Timestamp, event: &Event) -> Vec<u8> {
    serde_json::to_vec(&Payload {
        id,
        event: event.name(),
        at: at.as_millis(),
        data: event,
    })
    .expect("events serialize")
}

fn mac(secret: &[u8], timestamp: i64, body: &[u8]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC takes any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    mac
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The signature header's value for `body` sent at `timestamp` (Unix seconds):
/// `t=<timestamp>,v1=<hex HMAC-SHA256 of "<timestamp>.<body>" keyed with the secret>`.
pub fn sign(secret: &[u8], timestamp: i64, body: &[u8]) -> String {
    format!(
        "t={timestamp},v1={}",
        hex(&mac(secret, timestamp, body).finalize().into_bytes())
    )
}

/// Whether `header` signs `body` with `secret` and was made within `tolerance` of `now`
/// (Unix seconds). Compares in constant time. For receivers and tests.
pub fn verify(secret: &[u8], header: &str, body: &[u8], now: i64, tolerance: Duration) -> bool {
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", value)) => timestamp = value.parse::<i64>().ok(),
            Some(("v1", value)) => signatures.push(value.to_owned()),
            _ => {}
        }
    }
    let Some(timestamp) = timestamp else {
        return false;
    };
    if now.abs_diff(timestamp) > tolerance.as_secs() {
        return false;
    }
    signatures.iter().any(|signature| {
        let Some(bytes) = unhex(signature) else {
            return false;
        };
        mac(secret, timestamp, body).verify_slice(&bytes).is_ok()
    })
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// A new signing secret: `whsec_` and 64 hex digits from the system's random source.
pub fn new_secret() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the system's random source works");
    format!("whsec_{}", hex(&bytes))
}

/// Attempts made before a delivery is given up.
pub const MAX_ATTEMPTS: u32 = 6;

/// How long to wait after `attempts` failed attempts before the next; `None` once
/// [`MAX_ATTEMPTS`] are used up. 30 seconds, 2 minutes, 10 minutes, 1 hour, 6 hours.
pub fn retry_after(attempts: u32) -> Option<Duration> {
    const WAITS: [u64; 5] = [30, 120, 600, 3_600, 21_600];
    if attempts == 0 || attempts >= MAX_ATTEMPTS {
        return None;
    }
    WAITS
        .get(attempts as usize - 1)
        .map(|secs| Duration::from_secs(*secs))
}

/// Whether a status code counts as delivered: any `2xx`.
pub fn delivered(status: u16) -> bool {
    (200..300).contains(&status)
}

#[cfg(test)]
mod tests;
