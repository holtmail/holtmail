//! A fake directory and the conformance suites every implementation runs.

use std::collections::BTreeMap;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::{
    AliasChange, AliasDetector, AliasDirectory, AliasDomain, AliasError, AliasHit, AliasOptions,
    Capabilities, Header, Mail, Mailbox, NewAlias, RemoteAlias,
};
use holtmail_core::Address;

/// An in-memory alias account with every capability, on the domain `alias.example`, with two
/// mailboxes `1` (`owner@example.org`) and `2` (`other@example.org`).
pub struct FakeDirectory {
    capabilities: Capabilities,
    state: Mutex<FakeState>,
}

#[derive(Default)]
struct FakeState {
    aliases: BTreeMap<String, RemoteAlias>,
    next: u64,
    refuse_key: bool,
}

impl Default for FakeDirectory {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeDirectory {
    /// An empty account with every capability.
    pub fn new() -> Self {
        Self::with_capabilities(Capabilities {
            create_random: true,
            create_custom: true,
            note: true,
            name: true,
            for_domain: true,
            switch: true,
            mailboxes: true,
            delete: true,
        })
    }

    /// An empty account that can do only what `capabilities` says.
    pub fn with_capabilities(capabilities: Capabilities) -> Self {
        Self {
            capabilities,
            state: Mutex::new(FakeState::default()),
        }
    }

    /// Adds an alias as if made elsewhere and returns its id.
    pub async fn seed(&self, address: &str) -> String {
        let mut state = self.state.lock().await;
        state.next += 1;
        let id = state.next.to_string();
        state.aliases.insert(
            id.clone(),
            RemoteAlias {
                id: id.clone(),
                address: address.to_ascii_lowercase(),
                enabled: true,
                note: None,
                name: None,
                for_domain: None,
                mailboxes: vec![mailboxes()[0].clone()],
                created_at: None,
                forwarded: Some(0),
                blocked: Some(0),
                replied: Some(0),
            },
        );
        id
    }

    /// From now on every call fails with `Auth`.
    pub async fn refuse_key(&self) {
        self.state.lock().await.refuse_key = true;
    }

    async fn checked(&self) -> Result<tokio::sync::MutexGuard<'_, FakeState>, AliasError> {
        let state = self.state.lock().await;
        if state.refuse_key {
            return Err(AliasError::Auth);
        }
        Ok(state)
    }
}

fn mailboxes() -> Vec<Mailbox> {
    vec![
        Mailbox {
            id: "1".into(),
            email: "owner@example.org".into(),
        },
        Mailbox {
            id: "2".into(),
            email: "other@example.org".into(),
        },
    ]
}

fn chosen(ids: &[String]) -> Result<Vec<Mailbox>, AliasError> {
    if ids.is_empty() {
        return Ok(vec![mailboxes()[0].clone()]);
    }
    ids.iter()
        .map(|id| {
            mailboxes()
                .into_iter()
                .find(|m| &m.id == id)
                .ok_or_else(|| AliasError::Invalid(format!("no mailbox {id}")))
        })
        .collect()
}

fn text(value: &Option<String>) -> Option<String> {
    value.clone().filter(|v| !v.is_empty())
}

#[async_trait]
impl AliasDirectory for FakeDirectory {
    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    async fn aliases(&self) -> Result<Vec<RemoteAlias>, AliasError> {
        Ok(self.checked().await?.aliases.values().cloned().collect())
    }

    async fn options(&self) -> Result<AliasOptions, AliasError> {
        let _state = self.checked().await?;
        Ok(AliasOptions {
            domains: vec![AliasDomain {
                id: "alias.example".into(),
                label: "@alias.example".into(),
            }],
            mailboxes: mailboxes(),
            prefix_hint: None,
            can_create: true,
        })
    }

    async fn create(&self, new: &NewAlias) -> Result<RemoteAlias, AliasError> {
        let mut state = self.checked().await?;
        let custom = new.prefix.is_some();
        if (custom && !self.capabilities.create_custom)
            || (!custom && !self.capabilities.create_random)
        {
            return Err(AliasError::Unsupported);
        }
        if let Some(domain) = &new.domain
            && domain != "alias.example"
        {
            return Err(AliasError::Invalid(format!("no domain {domain}")));
        }
        state.next += 1;
        let id = state.next.to_string();
        let address = match &new.prefix {
            Some(prefix) => format!("{}@alias.example", prefix.to_ascii_lowercase()),
            None => format!("random{id}@alias.example"),
        };
        if state.aliases.values().any(|a| a.address == address) {
            return Err(AliasError::Invalid("this alias is taken".into()));
        }
        let alias = RemoteAlias {
            id: id.clone(),
            address,
            enabled: true,
            note: text(&new.note),
            name: text(&new.name),
            for_domain: text(&new.for_domain),
            mailboxes: chosen(&new.mailbox_ids)?,
            created_at: Some(holtmail_core::Timestamp::now()),
            forwarded: Some(0),
            blocked: Some(0),
            replied: Some(0),
        };
        state.aliases.insert(id, alias.clone());
        Ok(alias)
    }

    async fn update(&self, id: &str, change: &AliasChange) -> Result<RemoteAlias, AliasError> {
        let mut state = self.checked().await?;
        let c = self.capabilities;
        if (change.enabled.is_some() && !c.switch)
            || (change.note.is_some() && !c.note)
            || (change.name.is_some() && !c.name)
            || (change.for_domain.is_some() && !c.for_domain)
            || (change.mailbox_ids.is_some() && !c.mailboxes)
        {
            return Err(AliasError::Unsupported);
        }
        let mailboxes = match &change.mailbox_ids {
            Some(ids) if ids.is_empty() => {
                return Err(AliasError::Invalid("at least one mailbox".into()));
            }
            Some(ids) => Some(chosen(ids)?),
            None => None,
        };
        let alias = state.aliases.get_mut(id).ok_or(AliasError::NotFound)?;
        if let Some(enabled) = change.enabled {
            alias.enabled = enabled;
        }
        if change.note.is_some() {
            alias.note = text(&change.note);
        }
        if change.name.is_some() {
            alias.name = text(&change.name);
        }
        if change.for_domain.is_some() {
            alias.for_domain = text(&change.for_domain);
        }
        if let Some(mailboxes) = mailboxes {
            alias.mailboxes = mailboxes;
        }
        Ok(alias.clone())
    }

    async fn delete(&self, id: &str) -> Result<(), AliasError> {
        let mut state = self.checked().await?;
        if !self.capabilities.delete {
            return Err(AliasError::Unsupported);
        }
        state
            .aliases
            .remove(id)
            .map(|_| ())
            .ok_or(AliasError::NotFound)
    }
}

/// Builds a [`Mail`] from header fields; `From`, `To`, `Cc` and `Reply-To` are also parsed into
/// the address fields, as a message parser would.
pub fn mail(headers: &[(&str, &str)]) -> Mail {
    let mut mail = Mail {
        headers: headers.iter().map(|(n, v)| Header::new(*n, *v)).collect(),
        ..Mail::default()
    };
    let list = |value: &str| -> Vec<Address> {
        value
            .split(">,")
            .filter_map(|part| {
                let part = if part.contains('<') && !part.contains('>') {
                    format!("{part}>")
                } else {
                    part.to_owned()
                };
                crate::parse_mailbox(&part)
            })
            .collect()
    };
    for (name, value) in headers {
        match name.to_ascii_lowercase().as_str() {
            "from" => mail.from.extend(list(value)),
            "to" => mail.to.extend(list(value)),
            "cc" => mail.cc.extend(list(value)),
            "reply-to" => mail.reply_to.extend(list(value)),
            _ => {}
        }
    }
    mail
}

/// Conformance suites.
pub mod conformance {
    use super::*;

    /// One message and what the detector must find in it.
    pub struct Case {
        /// What the case shows, printed when it fails.
        pub name: &'static str,
        /// The message.
        pub mail: Mail,
        /// The expected answer.
        pub expected: Option<AliasHit>,
    }

    /// Checks a detector against the contract and against `cases`: plain mail and empty mail
    /// give `None`; every hit names the detector's provider and a lowercase alias with a domain;
    /// the same mail always gives the same answer; malformed fields never panic.
    pub fn detector(detector: &dyn AliasDetector, cases: &[Case]) {
        let provider = detector.provider();
        assert!(
            !provider.is_empty()
                && provider
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "provider id {provider:?} is lowercase ASCII"
        );
        assert_eq!(detector.version(), detector.version(), "version is stable");
        assert_eq!(detector.detect(&Mail::default()), None, "empty mail");
        let plain = mail(&[
            ("Received", "from mx.example.net by mx.example.org"),
            ("From", "Ada Lovelace <ada@example.com>"),
            ("To", "Bob <bob@example.org>"),
            ("Subject", "Lunch"),
            (
                "Authentication-Results",
                "mx.example.org; dkim=pass header.d=example.com",
            ),
        ]);
        assert_eq!(detector.detect(&plain), None, "plain mail");
        let broken = mail(&[
            ("From", "<<>>@@"),
            ("To", "\"unterminated <"),
            ("Reply-To", "@"),
            ("Authentication-Results", ";;; dkim=pass header.d="),
            ("X-SimpleLogin-Envelope-To", "<"),
            ("X-AnonAddy-Original-Sender", ""),
        ]);
        let _ = detector.detect(&broken);
        for case in cases {
            let found = detector.detect(&case.mail);
            assert_eq!(found, case.expected, "case {}", case.name);
            assert_eq!(
                found,
                detector.detect(&case.mail),
                "case {} again",
                case.name
            );
            if let Some(hit) = found {
                assert_eq!(hit.provider, provider, "case {}", case.name);
                assert_eq!(
                    hit.alias,
                    hit.alias.to_ascii_lowercase(),
                    "case {}",
                    case.name
                );
                assert!(hit.alias.contains('@'), "case {}", case.name);
                if let Some(reverse) = &hit.reverse_alias {
                    assert_eq!(reverse, &reverse.to_ascii_lowercase(), "case {}", case.name);
                }
            }
        }
    }

    /// Checks a directory against the contract. The account must be empty, or hold no alias
    /// made by this suite, and every created alias is deleted again when the directory can
    /// delete.
    pub async fn directory(directory: &dyn AliasDirectory) {
        let caps = directory.capabilities();
        let options = directory.options().await.expect("options");
        let before = directory.aliases().await.expect("aliases");

        let random = if caps.create_random {
            let made = directory
                .create(&NewAlias {
                    note: caps.note.then(|| "made by the suite".to_owned()),
                    ..NewAlias::default()
                })
                .await
                .expect("create random");
            assert_eq!(made.address, made.address.to_ascii_lowercase());
            assert!(made.enabled, "new aliases forward");
            if caps.note {
                assert_eq!(made.note.as_deref(), Some("made by the suite"));
            }
            Some(made)
        } else {
            assert_eq!(
                directory.create(&NewAlias::default()).await,
                Err(AliasError::Unsupported)
            );
            None
        };

        let custom = if caps.create_custom {
            let domain = options
                .domains
                .first()
                .expect("a domain for custom aliases");
            let made = directory
                .create(&NewAlias {
                    prefix: Some("suite_custom".into()),
                    domain: Some(domain.id.clone()),
                    ..NewAlias::default()
                })
                .await
                .expect("create custom");
            assert!(made.address.starts_with("suite_custom"), "{}", made.address);
            Some(made)
        } else {
            assert_eq!(
                directory
                    .create(&NewAlias {
                        prefix: Some("suite_custom".into()),
                        ..NewAlias::default()
                    })
                    .await,
                Err(AliasError::Unsupported)
            );
            None
        };

        let listed = directory.aliases().await.expect("aliases after create");
        for made in random.iter().chain(&custom) {
            assert_eq!(
                listed.iter().filter(|a| a.id == made.id).count(),
                1,
                "a new alias is listed once"
            );
        }
        let ids: std::collections::HashSet<_> = listed.iter().map(|a| &a.id).collect();
        assert_eq!(ids.len(), listed.len(), "every alias once");
        assert_eq!(
            listed.len(),
            before.len() + random.iter().count() + custom.iter().count()
        );

        let Some(target) = random.or(custom.clone()) else {
            return;
        };
        if caps.switch {
            let off = directory
                .update(
                    &target.id,
                    &AliasChange {
                        enabled: Some(false),
                        ..AliasChange::default()
                    },
                )
                .await
                .expect("switch off");
            assert!(!off.enabled);
            let listed = directory.aliases().await.expect("aliases");
            assert!(!listed.iter().find(|a| a.id == target.id).unwrap().enabled);
            let on = directory
                .update(
                    &target.id,
                    &AliasChange {
                        enabled: Some(true),
                        ..AliasChange::default()
                    },
                )
                .await
                .expect("switch on");
            assert!(on.enabled);
        } else {
            assert_eq!(
                directory
                    .update(
                        &target.id,
                        &AliasChange {
                            enabled: Some(false),
                            ..AliasChange::default()
                        },
                    )
                    .await,
                Err(AliasError::Unsupported)
            );
        }
        if caps.note {
            let noted = directory
                .update(
                    &target.id,
                    &AliasChange {
                        note: Some("changed".into()),
                        ..AliasChange::default()
                    },
                )
                .await
                .expect("note");
            assert_eq!(noted.note.as_deref(), Some("changed"));
            let cleared = directory
                .update(
                    &target.id,
                    &AliasChange {
                        note: Some(String::new()),
                        ..AliasChange::default()
                    },
                )
                .await
                .expect("clear note");
            assert_eq!(cleared.note, None);
        }
        if caps.mailboxes && options.mailboxes.len() > 1 {
            let moved = directory
                .update(
                    &target.id,
                    &AliasChange {
                        mailbox_ids: Some(vec![options.mailboxes[1].id.clone()]),
                        ..AliasChange::default()
                    },
                )
                .await
                .expect("mailboxes");
            assert_eq!(moved.mailboxes, vec![options.mailboxes[1].clone()]);
        }
        let probe = if caps.switch {
            Some(AliasChange {
                enabled: Some(true),
                ..AliasChange::default()
            })
        } else if caps.note {
            Some(AliasChange {
                note: Some("x".into()),
                ..AliasChange::default()
            })
        } else {
            None
        };
        if let Some(probe) = probe {
            assert_eq!(
                directory.update("no-such-alias-0", &probe).await,
                Err(AliasError::NotFound)
            );
        }

        if caps.delete {
            let other = custom.filter(|c| c.id != target.id);
            for made in [Some(target), other].into_iter().flatten() {
                directory.delete(&made.id).await.expect("delete");
                assert_eq!(directory.delete(&made.id).await, Err(AliasError::NotFound));
            }
            let after = directory.aliases().await.expect("aliases after delete");
            assert_eq!(after.len(), before.len(), "deleted aliases are not listed");
        } else {
            assert_eq!(
                directory.delete(&target.id).await,
                Err(AliasError::Unsupported)
            );
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn the_fake_keeps_the_contract() {
            directory(&FakeDirectory::new()).await;
            directory(&FakeDirectory::with_capabilities(Capabilities {
                create_random: true,
                switch: true,
                ..Capabilities::default()
            }))
            .await;
            directory(&FakeDirectory::with_capabilities(Capabilities {
                create_custom: true,
                note: true,
                delete: true,
                ..Capabilities::default()
            }))
            .await;
        }

        #[tokio::test]
        async fn a_refused_key_is_auth() {
            let fake = FakeDirectory::new();
            fake.refuse_key().await;
            assert_eq!(fake.options().await, Err(AliasError::Auth));
            assert_eq!(fake.aliases().await, Err(AliasError::Auth));
        }
    }
}
