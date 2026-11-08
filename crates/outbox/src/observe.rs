use holtmail_blob::BlobId;
use holtmail_core::{ArchiveId, FolderId, MessageId, OutboxId};
use holtmail_mime::{Dsn, DsnAction, ParsedMessage};
use holtmail_store::{
    AuditKind, FolderRole, NewAuditEntry, OutboxChange, OutboxEntry, OutboxRecipient, OutboxState,
    RecipientState, RewrittenFrom,
};

use crate::{Outbox, OutboxError};

/// States in which a message was handed to its transport and may have left.
const HANDED_OVER: [OutboxState; 3] = [
    OutboxState::Sent,
    OutboxState::Bounced,
    OutboxState::Uncertain,
];

/// A message sync just added to an archive.
#[derive(Debug, Clone, Copy)]
pub struct Arrival<'a> {
    /// The archive it was added to.
    pub archive: ArchiveId,
    /// The new message.
    pub message: MessageId,
    /// Its bytes.
    pub raw: &'a [u8],
    /// The same, parsed.
    pub parsed: &'a ParsedMessage,
    /// The folder it was found in.
    pub folder: Option<FolderId>,
    /// Its labels, for providers with labels.
    pub labels: Option<&'a [String]>,
    /// It arrived while the folder was followed, not in a backfill or an import.
    pub live: bool,
}

/// What [`Outbox::observe`] learned from one arrival.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// Outbox messages the arrival is now the synced copy of.
    pub linked: Vec<OutboxId>,
    /// Outbox messages a failure report marked bounced.
    pub bounced: Vec<OutboxId>,
    /// Outbox messages a delay report put a note on.
    pub delayed: Vec<OutboxId>,
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn bare(id: &str) -> &str {
    id.trim().trim_start_matches('<').trim_end_matches('>')
}

impl Outbox {
    /// Reads a message sync just added for what it says about sent mail.
    ///
    /// - **Sent copies.** When it sits in a Sent folder (or carries Gmail's `Sent` label), has
    ///   the `Message-ID` of outbox messages of the same archive that were handed over, and is
    ///   not their sent bytes, it becomes their [`OutboxEntry::synced_copy`]: the provider's
    ///   copy of the same message. Its `From` is compared with the identity's address: another
    ///   address flags the identity with [`RewrittenFrom`], the same address clears the flag.
    /// - **Bounces**, only for live arrivals. A delivery status notification (RFC 3464, or an
    ///   unstructured bounce from `MAILER-DAEMON` that quotes the original) about a sent
    ///   `Message-ID` of the same archive marks the outbox messages
    ///   [`OutboxState::Bounced`]: every failed recipient that names an envelope recipient
    ///   becomes [`RecipientState::Bounced`] with its status and diagnostic. A report naming no
    ///   recipient counts for the only accepted recipient, or marks the message bounced without
    ///   naming anyone when there were several. Every bounce is an audit entry. A delay report
    ///   only sets a note.
    ///
    /// Idempotent: the same arrival again changes nothing more. Errors are the store's; nothing
    /// is logged but ids and counts.
    pub async fn observe(&self, arrival: Arrival<'_>) -> Result<Observed, OutboxError> {
        let mut observed = Observed::default();
        if self.in_sent(&arrival).await?
            && let Some(id) = arrival.parsed.message_id.as_deref()
        {
            let blob = BlobId::of(arrival.raw);
            for entry in self
                .store
                .outbox_by_rfc_message_id(arrival.archive, bare(id))
                .await?
            {
                if !HANDED_OVER.contains(&entry.state)
                    || entry.new.blob_id == blob
                    || entry.message_id == Some(arrival.message)
                {
                    continue;
                }
                if entry.synced_copy != Some(arrival.message) {
                    self.store
                        .link_outbox_copy(entry.id, arrival.message, self.now())
                        .await?;
                    tracing::info!(outbox = %entry.id, message = %arrival.message,
                        "synced sent copy linked");
                }
                self.check_from(&entry, arrival.parsed).await?;
                observed.linked.push(entry.id);
            }
        }
        if arrival.live
            && let Some(report) = holtmail_mime::parse_dsn(arrival.raw)
                .or_else(|| holtmail_mime::parse_unstructured_bounce(arrival.raw))
        {
            self.read_report(&arrival, &report, &mut observed).await?;
        }
        Ok(observed)
    }

    async fn in_sent(&self, arrival: &Arrival<'_>) -> Result<bool, OutboxError> {
        if arrival
            .labels
            .is_some_and(|labels| labels.iter().any(|l| same(l, "Sent")))
        {
            return Ok(true);
        }
        let Some(folder) = arrival.folder else {
            return Ok(false);
        };
        Ok(self
            .store
            .get_folder(folder)
            .await?
            .is_some_and(|f| f.role == Some(FolderRole::Sent)))
    }

    async fn check_from(
        &self,
        entry: &OutboxEntry,
        parsed: &ParsedMessage,
    ) -> Result<(), OutboxError> {
        let Some(identity) = self.store.get_identity(entry.new.identity_id).await? else {
            return Ok(());
        };
        let Some(from) = parsed.from.first().map(|a| a.email.as_str()) else {
            return Ok(());
        };
        if same(from, &identity.fields.address) {
            if identity.rewritten_from.is_some() {
                self.store
                    .set_identity_rewritten_from(identity.id, None)
                    .await?;
            }
            return Ok(());
        }
        if identity
            .rewritten_from
            .as_ref()
            .is_some_and(|r| same(&r.seen, from))
        {
            return Ok(());
        }
        self.store
            .set_identity_rewritten_from(
                identity.id,
                Some(&RewrittenFrom {
                    seen: from.to_lowercase(),
                    at: self.now(),
                }),
            )
            .await?;
        tracing::warn!(identity = %identity.id, outbox = %entry.id,
            "the provider sent the message with another From");
        Ok(())
    }

    async fn read_report(
        &self,
        arrival: &Arrival<'_>,
        report: &Dsn,
        observed: &mut Observed,
    ) -> Result<(), OutboxError> {
        let Some(original) = report.original_message_id.as_deref().map(bare) else {
            return Ok(());
        };
        if original.is_empty() {
            return Ok(());
        }
        for entry in self
            .store
            .outbox_by_rfc_message_id(arrival.archive, original)
            .await?
        {
            if !HANDED_OVER.contains(&entry.state) || entry.message_id == Some(arrival.message) {
                continue;
            }
            if self.bounce(&entry, report).await? {
                observed.bounced.push(entry.id);
            } else if self.delay(&entry, report).await? {
                observed.delayed.push(entry.id);
            }
        }
        Ok(())
    }

    async fn bounce(&self, entry: &OutboxEntry, report: &Dsn) -> Result<bool, OutboxError> {
        let failed: Vec<_> = report
            .recipients
            .iter()
            .filter(|r| r.action == Some(DsnAction::Failed))
            .collect();
        if failed.is_empty() {
            return Ok(false);
        }
        let mut recipients = entry.recipients.clone();
        let mut named = 0;
        let mut unnamed = false;
        for failure in &failed {
            let at = recipients.iter().position(|r| {
                [&failure.final_recipient, &failure.original_recipient]
                    .into_iter()
                    .flatten()
                    .any(|a| same(a, &r.address))
            });
            let at = match at {
                Some(at) => Some(at),
                None if failure.final_recipient.is_none()
                    && failure.original_recipient.is_none() =>
                {
                    unnamed = true;
                    let accepted: Vec<usize> = recipients
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| r.state == RecipientState::Accepted)
                        .map(|(i, _)| i)
                        .collect();
                    (accepted.len() == 1).then(|| accepted[0])
                }
                None => None,
            };
            if let Some(at) = at {
                recipients[at] = OutboxRecipient {
                    address: recipients[at].address.clone(),
                    state: RecipientState::Bounced {
                        status: failure.status.clone(),
                        text: failure.diagnostic_code.clone(),
                    },
                };
                named += 1;
            }
        }
        if named == 0 && !unnamed {
            return Ok(false);
        }
        if entry.state == OutboxState::Bounced && recipients == entry.recipients {
            return Ok(false);
        }
        self.store
            .append_audit(&NewAuditEntry {
                at: self.now(),
                user: None,
                kind: AuditKind::OutboxChanged,
                archive: Some(entry.new.archive_id),
                account: None,
                targets: vec![entry.id.to_string(), "bounced".into()],
                count: named.max(1) as u64,
            })
            .await?;
        let change = OutboxChange {
            recipients: Some(recipients),
            error: entry.last_error.clone(),
            next_attempt_at: entry.next_attempt_at,
            ..OutboxChange::to(OutboxState::Bounced, self.now())
        };
        let changed = self
            .store
            .change_outbox(entry.id, &HANDED_OVER, &change)
            .await?;
        if changed.is_some() {
            tracing::info!(outbox = %entry.id, recipients = named, "bounce recorded");
            if named == 0 {
                self.store
                    .set_outbox_note(
                        entry.id,
                        Some("A bounce arrived that does not say which recipient it is about."),
                        self.now(),
                    )
                    .await?;
            }
        }
        Ok(changed.is_some())
    }

    async fn delay(&self, entry: &OutboxEntry, report: &Dsn) -> Result<bool, OutboxError> {
        let delayed: Vec<String> = report
            .recipients
            .iter()
            .filter(|r| r.action == Some(DsnAction::Delayed))
            .filter_map(|r| {
                let address = r.final_recipient.as_deref()?;
                entry
                    .recipients
                    .iter()
                    .find(|e| same(&e.address, address))
                    .map(|e| match &r.status {
                        Some(status) => format!("{} ({status})", e.address),
                        None => e.address.clone(),
                    })
            })
            .collect();
        if delayed.is_empty() {
            return Ok(false);
        }
        let note = format!(
            "Delivery is delayed; the receiving server is still trying: {}.",
            delayed.join(", ")
        );
        if entry.note.as_deref() == Some(note.as_str()) {
            return Ok(false);
        }
        self.store
            .set_outbox_note(entry.id, Some(&note), self.now())
            .await?;
        tracing::info!(outbox = %entry.id, recipients = delayed.len(), "delay report recorded");
        Ok(true)
    }
}
