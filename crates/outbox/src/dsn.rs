use holtmail_core::Timestamp;
use holtmail_store::{OutboxEntry, OutboxState, RecipientState};
use mail_builder::headers::date::Date;

use crate::Outbox;

const BOUNDARY: &str = "holtmail-delivery-report";
const NOTE: &str = "delivery failed; a delivery status report is in the archive";

struct Undelivered<'a> {
    address: &'a str,
    status: String,
    diagnostic: String,
}

fn undelivered(entry: &OutboxEntry) -> Vec<Undelivered<'_>> {
    let fallback = entry
        .last_error
        .as_deref()
        .unwrap_or("the message could not be sent");
    entry
        .recipients
        .iter()
        .filter_map(|recipient| match &recipient.state {
            RecipientState::Accepted | RecipientState::Bounced { .. } => None,
            RecipientState::Refused { code, text, .. } => Some(Undelivered {
                address: &recipient.address,
                status: format!("{}.0.0", code / 100),
                diagnostic: format!("{code} {text}"),
            }),
            RecipientState::Pending => (entry.state == OutboxState::Failed).then(|| Undelivered {
                address: &recipient.address,
                status: "5.0.0".to_owned(),
                diagnostic: fallback.to_owned(),
            }),
        })
        .collect()
}

fn field(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The header of `message`, up to and not including the empty line.
fn header_block(message: &[u8]) -> &[u8] {
    let end = message
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(message.len(), |at| at + 2);
    &message[..end]
}

fn domain_of(address: &str) -> &str {
    address.rsplit_once('@').map_or("invalid", |(_, d)| d)
}

fn report(
    entry: &OutboxEntry,
    failed: &[Undelivered<'_>],
    original: &[u8],
    at: Timestamp,
) -> Vec<u8> {
    let sender = &entry.new.mail_from;
    let domain = domain_of(sender);
    let mut out = format!(
        "From: Mail Delivery System <mailer-daemon@{domain}>\r\n\
         To: <{sender}>\r\n\
         Subject: Undelivered mail returned to sender\r\n\
         Date: {date}\r\n\
         Message-ID: <{id}.{attempts}.delivery-report@{domain}>\r\n\
         Auto-Submitted: auto-replied\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/report; report-type=delivery-status;\r\n \
         boundary=\"{BOUNDARY}\"\r\n\r\n\
         --{BOUNDARY}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n\
         Holtmail took this message from your mail client, but could not deliver it to:\r\n",
        date = Date::new(at.as_secs()).to_rfc822(),
        id = entry.id,
        attempts = entry.attempts,
    );
    for one in failed {
        out.push_str(&format!(
            "\r\n  {}\r\n    {}\r\n",
            field(one.address),
            field(&one.diagnostic)
        ));
    }
    out.push_str(&format!(
        "\r\n--{BOUNDARY}\r\n\
         Content-Type: message/delivery-status\r\n\r\n\
         Reporting-MTA: dns; {domain}\r\n\
         Original-Envelope-Id: {id}\r\n\r\n",
        id = entry.id,
    ));
    for one in failed {
        out.push_str(&format!(
            "Final-Recipient: rfc822; {}\r\n\
             Action: failed\r\n\
             Status: {}\r\n\
             Diagnostic-Code: smtp; {}\r\n\r\n",
            field(one.address),
            one.status,
            field(&one.diagnostic),
        ));
    }
    out.push_str(&format!(
        "--{BOUNDARY}\r\nContent-Type: text/rfc822-headers\r\n\r\n"
    ));
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(header_block(original));
    bytes.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    bytes
}

impl Outbox {
    /// Tells a mail client's user about a message that was accepted from a device and then
    /// could not be delivered to someone: an RFC 3464 report is archived and the outbox row says
    /// so. Runs after a message failed for good or was sent with refused recipients. Anything
    /// that goes wrong here is logged and leaves the row as it is.
    pub(crate) async fn report_undelivered(&self, entry: &OutboxEntry) {
        if entry.new.device_id.is_none()
            || !matches!(entry.state, OutboxState::Failed | OutboxState::Sent)
        {
            return;
        }
        let failed = undelivered(entry);
        if failed.is_empty() {
            return;
        }
        let original = match self.blobs.get(&entry.new.blob_id).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => Default::default(),
            Err(error) => {
                tracing::warn!(outbox = %entry.id, %error, "delivery report not made");
                return;
            }
        };
        let bytes = report(entry, &failed, &original, self.now());
        if let Err(error) = self
            .archiver
            .archive(entry.new.archive_id, bytes.into(), None, &[])
            .await
        {
            tracing::warn!(outbox = %entry.id, %error, "delivery report not archived");
            return;
        }
        tracing::info!(outbox = %entry.id, recipients = failed.len(), "delivery report archived");
        if let Err(error) = self
            .store
            .set_outbox_note(entry.id, Some(NOTE), self.now())
            .await
        {
            tracing::warn!(outbox = %entry.id, %error, "delivery notice not recorded");
        }
    }
}
