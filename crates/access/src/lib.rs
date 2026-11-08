//! The one decision for every write: the instance switch, the account's capability, the user's
//! grant on the archive, the scope of the credentials and, for mail on a server that asks for
//! it, the consent of the account's sign-in. Every lock must be open. A store failure or an
//! unknown account refuses, so the decision fails closed.
//!
//! The web API and every protocol front end ask this crate, so a write cannot be allowed through
//! one door and refused at another.

use std::collections::BTreeSet;
use std::sync::Arc;

use holtmail_core::{AccountId, ArchiveId, DeviceScope};
use holtmail_store::{
    Account, AccountKind, Capabilities, Capability, GrantRole, InstanceRole, Store, StoreError,
    Transport, TransportKind, User,
};

/// The capabilities that change mail on an account's server.
pub const MAIL_WRITE_CAPABILITIES: [Capability; 5] = [
    Capability::MailFlags,
    Capability::MailMove,
    Capability::MailDelete,
    Capability::MailExpunge,
    Capability::MailAppend,
];

/// Whether any capability that changes mail on the server is on.
pub fn writes_mail(capabilities: &Capabilities) -> bool {
    MAIL_WRITE_CAPABILITIES.iter().any(|c| capabilities.has(*c))
}

/// Whether the account's sign-in lacks the consent its server needs before mail can be changed
/// there: a Microsoft Graph account signed in with OAuth (`settings.oauth`) whose last sign-in
/// did not grant `Mail.ReadWrite` (`settings.oauth.write` is not `true`). Other accounts, and
/// Graph accounts with a token of their own, need nothing more.
pub fn write_consent_missing(account: &Account) -> bool {
    account.kind == AccountKind::Graph
        && account.settings.get("oauth").is_some_and(|oauth| {
            oauth.get("write").and_then(serde_json::Value::as_bool) != Some(true)
        })
}

/// The instance switches from the `[write]` config. A switch that is off turns its capabilities
/// off for every account, whatever is stored; all are on by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteSwitches {
    /// Any write to a mail, contacts or calendar server.
    pub enabled: bool,
    /// Sending mail; needs `enabled` too.
    pub send: bool,
    /// Running rule flows; needs `enabled` too.
    pub rules: bool,
}

impl Default for WriteSwitches {
    fn default() -> Self {
        Self {
            enabled: true,
            send: true,
            rules: true,
        }
    }
}

impl WriteSwitches {
    /// Whether the switches leave `capability` possible at all.
    pub fn allow(self, capability: Capability) -> bool {
        let (_, _, send) = needs(capability);
        self.enabled && (!send || self.send)
    }
}

/// What credentials allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Reading archives.
    Read,
    /// Flags, folders, deleting and appending on mail servers.
    WriteMail,
    /// Sending mail.
    SendMail,
    /// Changing contacts and calendars upstream and answering invitations.
    WritePim,
    /// Saving and running rule flows.
    Rules,
}

impl Scope {
    /// Every scope, in a fixed order.
    pub const ALL: [Scope; 5] = [
        Self::Read,
        Self::WriteMail,
        Self::SendMail,
        Self::WritePim,
        Self::Rules,
    ];

    /// The stable name: `read`, `write:mail`, `send:mail`, `write:pim` or `rules`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::WriteMail => "write:mail",
            Self::SendMail => "send:mail",
            Self::WritePim => "write:pim",
            Self::Rules => "rules",
        }
    }

    /// The scope named by [`Scope::as_str`], or `None`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == name)
    }
}

/// The scopes one request or session carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scopes(BTreeSet<Scope>);

impl Scopes {
    /// Every scope, as a cookie session has.
    pub fn all() -> Self {
        Self(Scope::ALL.into())
    }

    /// Only these scopes.
    pub fn only(scopes: impl IntoIterator<Item = Scope>) -> Self {
        Self(scopes.into_iter().collect())
    }

    /// What a device with these scopes may do. A device never has `rules`, and `notify` opens no
    /// write.
    pub fn of_device(scopes: &BTreeSet<DeviceScope>) -> Self {
        Self(
            scopes
                .iter()
                .filter_map(|scope| match scope {
                    DeviceScope::Read => Some(Scope::Read),
                    DeviceScope::WriteMail => Some(Scope::WriteMail),
                    DeviceScope::SendMail => Some(Scope::SendMail),
                    DeviceScope::WritePim => Some(Scope::WritePim),
                    DeviceScope::Notify => None,
                })
                .collect(),
        )
    }

    /// Whether `scope` is among them.
    pub fn has(&self, scope: Scope) -> bool {
        self.0.contains(&scope)
    }
}

/// Which lock refused a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    /// An instance switch is off.
    Switch,
    /// The account does not allow it.
    Capability,
    /// The user's grant on the archive does not allow it.
    Grant,
    /// The credentials do not allow it.
    Scope,
}

impl Denial {
    /// A sentence for the person refused.
    pub fn message(self) -> &'static str {
        match self {
            Self::Switch => "writing is turned off on this instance",
            Self::Capability => "this account does not allow this change",
            Self::Grant => "your grant on this archive does not allow this change",
            Self::Scope => "these credentials do not allow this change",
        }
    }
}

/// Why a check refused.
#[derive(Debug, thiserror::Error)]
pub enum AccessError {
    /// The account is unknown, or sits in an archive the user may not read: `"account"` or
    /// `"archive"`, whichever was asked for. Reported as if it did not exist.
    #[error("no such {0}")]
    NotFound(&'static str),
    /// A lock is closed.
    #[error("{}", .0.message())]
    Denied(Denial),
    /// Every lock is open, but the account's sign-in lacks the consent to change mail.
    #[error("the account's sign-in does not allow changing mail; sign in again")]
    ConsentMissing,
    /// The store failed, so nothing is allowed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What a capability needs besides itself: the grant role, the scope, and whether the `send`
/// switch must be on.
fn needs(capability: Capability) -> (GrantRole, Scope, bool) {
    match capability {
        Capability::MailSend => (GrantRole::Sender, Scope::SendMail, true),
        Capability::PimEdit | Capability::PimRespond => (GrantRole::Editor, Scope::WritePim, false),
        _ => (GrantRole::Editor, Scope::WriteMail, false),
    }
}

/// The decision on values already looked up. Every lock must be open; the first closed one is
/// reported, in the order switch, capability, grant, scope. `mail.expunge` also needs
/// `mail.delete`. `None` as `role` means no grant.
pub fn decide(
    switches: WriteSwitches,
    role: Option<GrantRole>,
    capabilities: &Capabilities,
    scopes: &Scopes,
    capability: Capability,
) -> Result<(), Denial> {
    let (role_needed, scope, send) = needs(capability);
    if !switches.enabled || (send && !switches.send) {
        return Err(Denial::Switch);
    }
    if !capabilities.has(capability)
        || (capability == Capability::MailExpunge && !capabilities.has(Capability::MailDelete))
    {
        return Err(Denial::Capability);
    }
    if role.is_none_or(|role| role < role_needed) {
        return Err(Denial::Grant);
    }
    if !scopes.has(scope) {
        return Err(Denial::Scope);
    }
    Ok(())
}

/// Answers who may write, from the store and the instance switches. Holds no state of its own,
/// so every answer reflects the store at the moment of the call. Safe to share between tasks.
#[derive(Clone)]
pub struct Access {
    store: Arc<dyn Store>,
    switches: WriteSwitches,
}

impl Access {
    /// Decides over `store` with these switches.
    pub fn new(store: Arc<dyn Store>, switches: WriteSwitches) -> Self {
        Self { store, switches }
    }

    /// The instance switches in force.
    pub fn switches(&self) -> WriteSwitches {
        self.switches
    }

    /// The user's role in the archive: the instance owner has every right, an admin reads every
    /// archive and writes only with a grant, a member has what their grant says. `None` when
    /// the user may not read the archive.
    pub async fn role_in(
        &self,
        user: &User,
        archive: ArchiveId,
    ) -> Result<Option<GrantRole>, StoreError> {
        if user.role == InstanceRole::Owner {
            return Ok(Some(GrantRole::Sender));
        }
        let grant = self
            .store
            .grant_roles(user.id)
            .await?
            .into_iter()
            .find(|g| g.archive == archive)
            .map(|g| g.role);
        Ok(match grant {
            None if user.role == InstanceRole::Admin => Some(GrantRole::Reader),
            other => other,
        })
    }

    /// The one check before anything changes on a server: the instance switch, the account's
    /// capability, the user's grant on the account's archive, the scope and, for mail, the
    /// sign-in's consent must all allow `capability`. Returns the account. An unknown account,
    /// or one in an archive the user may not read, is [`AccessError::NotFound`] with
    /// `"account"`.
    pub async fn may_write(
        &self,
        user: &User,
        scopes: &Scopes,
        account: AccountId,
        capability: Capability,
    ) -> Result<Account, AccessError> {
        let account = self
            .store
            .get_account(account)
            .await?
            .ok_or(AccessError::NotFound("account"))?;
        let role = self.role_in(user, account.archive_id).await?;
        if role.is_none() {
            return Err(AccessError::NotFound("account"));
        }
        let capabilities = self.store.account_capabilities(account.id).await?;
        decide(self.switches, role, &capabilities, scopes, capability)
            .map_err(AccessError::Denied)?;
        if MAIL_WRITE_CAPABILITIES.contains(&capability) && write_consent_missing(&account) {
            return Err(AccessError::ConsentMissing);
        }
        Ok(account)
    }

    /// The mailbox capabilities the user may use on the account now, as [`Access::may_write`]
    /// decides them, in the order of [`MAIL_WRITE_CAPABILITIES`]; empty for an import account
    /// and for an account whose sign-in lacks the consent to change mail.
    pub async fn writable(
        &self,
        user: &User,
        scopes: &Scopes,
        account: &Account,
    ) -> Result<Vec<Capability>, StoreError> {
        if account.kind == AccountKind::Import || write_consent_missing(account) {
            return Ok(Vec::new());
        }
        let role = self.role_in(user, account.archive_id).await?;
        let capabilities = self.store.account_capabilities(account.id).await?;
        Ok(MAIL_WRITE_CAPABILITIES
            .into_iter()
            .filter(|c| decide(self.switches, role, &capabilities, scopes, *c).is_ok())
            .collect())
    }

    /// The check every send makes: the instance switches, a `sender` grant, the `send:mail`
    /// scope and, for an account transport, that account's `mail.send`. A user who may not read
    /// the archive is [`AccessError::NotFound`] with `"archive"`.
    pub async fn may_send(
        &self,
        user: &User,
        scopes: &Scopes,
        archive: ArchiveId,
        transport: &Transport,
    ) -> Result<(), AccessError> {
        let Some(role) = self.role_in(user, archive).await? else {
            return Err(AccessError::NotFound("archive"));
        };
        let capabilities = match transport.kind {
            TransportKind::Account => match transport.account_id {
                Some(account) => self.store.account_capabilities(account).await?,
                None => Capabilities::none(),
            },
            TransportKind::Smtp | TransportKind::Jmap | TransportKind::Graph => {
                [Capability::MailSend].into_iter().collect()
            }
        };
        decide(
            self.switches,
            Some(role),
            &capabilities,
            scopes,
            Capability::MailSend,
        )
        .map_err(AccessError::Denied)
    }

    /// The locks of [`Access::may_send`] that do not depend on the transport: the instance
    /// switches, a `sender` grant and the `send:mail` scope. No grant is
    /// [`Denial::Grant`], not a missing archive.
    pub async fn may_send_somehow(
        &self,
        user: &User,
        scopes: &Scopes,
        archive: ArchiveId,
    ) -> Result<(), AccessError> {
        let role = self.role_in(user, archive).await?;
        decide(
            self.switches,
            role,
            &[Capability::MailSend].into_iter().collect(),
            scopes,
            Capability::MailSend,
        )
        .map_err(AccessError::Denied)
    }

    /// The check every rule endpoint makes: the `rules` switch, an editor grant on the archive
    /// and the `rules` scope. What a flow does is checked again with [`Access::may_write`] when
    /// it runs. A user who may not read the archive is [`AccessError::NotFound`] with
    /// `"archive"`.
    pub async fn may_use_rules(
        &self,
        user: &User,
        scopes: &Scopes,
        archive: ArchiveId,
    ) -> Result<(), AccessError> {
        let Some(role) = self.role_in(user, archive).await? else {
            return Err(AccessError::NotFound("archive"));
        };
        if !self.switches.enabled || !self.switches.rules {
            return Err(AccessError::Denied(Denial::Switch));
        }
        if role < GrantRole::Editor {
            return Err(AccessError::Denied(Denial::Grant));
        }
        if !scopes.has(Scope::Rules) {
            return Err(AccessError::Denied(Denial::Scope));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
