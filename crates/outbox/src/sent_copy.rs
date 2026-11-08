use holtmail_core::{AccountId, FolderId};
use holtmail_store::{
    AuditKind, Capability, FolderRole, MailChangeOrigin, MailOp, NewAuditEntry, NewMailChange,
    OutboxEntry, SentCopyPolicy, Transport,
};

use crate::{Outbox, OutboxError};

impl Outbox {
    /// Acts on the identity's sent-copy policy after a send: for `append:<folder>`, records an
    /// append of the sent bytes with `\Seen` to that folder of the identity's account (else its
    /// transport's account) in `mail_changes`, for `mail-writeback` to send and retry. Needs the
    /// account's `mail.append`; without it, or without the account or the folder, the message
    /// gets a note instead. `provider` and `none` need nothing. Never fails the send.
    pub(crate) async fn save_sent_copy(&self, entry: &OutboxEntry, transport: &Transport) {
        match self.sent_copy(entry, transport).await {
            Ok(None) => {}
            Ok(Some(note)) => {
                tracing::info!(outbox = %entry.id, "sent copy not saved upstream");
                if let Err(error) = self
                    .store
                    .set_outbox_note(entry.id, Some(note), self.now())
                    .await
                {
                    tracing::warn!(outbox = %entry.id, %error, "sent copy note not recorded");
                }
            }
            Err(error) => {
                tracing::warn!(outbox = %entry.id, %error, "sent copy not recorded");
            }
        }
    }

    async fn sent_copy(
        &self,
        entry: &OutboxEntry,
        transport: &Transport,
    ) -> Result<Option<&'static str>, OutboxError> {
        let Some(identity) = self.store.get_identity(entry.new.identity_id).await? else {
            return Ok(None);
        };
        let SentCopyPolicy::Append(name) = &identity.fields.sent_copy else {
            return Ok(None);
        };
        let Some(account) = identity.fields.account_id.or(transport.account_id) else {
            return Ok(Some(
                "The sent copy was not saved: the identity has no account.",
            ));
        };
        if !self
            .store
            .account_capabilities(account)
            .await?
            .has(Capability::MailAppend)
        {
            return Ok(Some(
                "The sent copy was not saved: the account does not allow saving mail.",
            ));
        }
        let Some(folder) = self.copy_folder(entry, account, name).await? else {
            return Ok(Some(
                "The sent copy was not saved: the folder was not found.",
            ));
        };
        let now = self.now();
        let change = self
            .store
            .enqueue_mail_change(&NewMailChange {
                account_id: account,
                folder_id: folder,
                message_id: entry.message_id,
                uid: None,
                op: MailOp::Append {
                    blob_id: entry.new.blob_id,
                    flags: vec!["\\Seen".into()],
                    date: entry.sent_at.unwrap_or(now),
                },
                origin: MailChangeOrigin::SentCopy,
                user_id: entry.new.user_id,
                device_id: entry.new.device_id,
                outbox_id: Some(entry.id),
                at: now,
            })
            .await?;
        let mut targets = vec![entry.id.to_string(), change.id.to_string()];
        targets.extend(entry.message_id.map(|m| m.to_string()));
        self.store
            .append_audit(&NewAuditEntry {
                at: now,
                user: entry.new.user_id,
                kind: AuditKind::MailAppend,
                archive: Some(entry.new.archive_id),
                account: Some(account),
                targets,
                count: 1,
            })
            .await?;
        tracing::info!(outbox = %entry.id, change = %change.id, "sent copy held for the server");
        Ok(None)
    }

    async fn copy_folder(
        &self,
        entry: &OutboxEntry,
        account: AccountId,
        name: &str,
    ) -> Result<Option<FolderId>, OutboxError> {
        let folders: Vec<_> = self
            .store
            .list_folders(entry.new.archive_id)
            .await?
            .into_iter()
            .map(|f| f.folder)
            .filter(|f| f.account_id == account)
            .collect();
        let found = folders
            .iter()
            .find(|f| f.path == name)
            .or_else(|| folders.iter().find(|f| f.path.eq_ignore_ascii_case(name)))
            .or_else(|| {
                folders
                    .iter()
                    .find(|f| f.role == Some(FolderRole::Sent) && name.eq_ignore_ascii_case("sent"))
            });
        Ok(found.map(|f| f.id))
    }
}
