use std::collections::{HashMap, HashSet};

use bytes::Bytes;
use holtmail_core::{MessageId, Timestamp};
use holtmail_index::{Clause, DocumentKind, MAX_MATCHES, SearchQuery};
use holtmail_mailview::{EmailRef, Filter, MailViewError, Member, TextField};

use crate::layout::Entry;
use crate::{ArchiveMailView, Ctx, mime, same, unavailable};

struct Facts {
    size: u64,
    received_at: Timestamp,
    sent_at: Option<Timestamp>,
    attachment: bool,
}

struct Lookup {
    facts: HashMap<EmailRef, Facts>,
    flags: HashMap<EmailRef, Vec<String>>,
    raw: HashMap<EmailRef, Bytes>,
    text: HashMap<(String, String), HashSet<MessageId>>,
}

fn day(t: Timestamp) -> i64 {
    t.as_millis().div_euclid(86_400_000)
}

fn clause(field: &TextField, value: &str) -> Option<Clause> {
    let value = value.to_string();
    Some(match field {
        TextField::Any => Clause::Text(value),
        TextField::Body => Clause::Body(value),
        TextField::Subject => Clause::Subject(value),
        TextField::From => Clause::From(value),
        TextField::To | TextField::Cc | TextField::Bcc => Clause::To(value),
        TextField::Header(_) => return None,
    })
}

fn text_key(field: &TextField, value: &str) -> (String, String) {
    (format!("{field:?}"), value.to_string())
}

fn needs_raw(filter: &Filter) -> bool {
    match filter {
        Filter::And(all) | Filter::Or(all) => all.iter().any(needs_raw),
        Filter::Not(inner) => needs_raw(inner),
        Filter::Text(TextField::Header(_), _) => true,
        _ => false,
    }
}

fn texts<'a>(filter: &'a Filter, out: &mut Vec<(&'a TextField, &'a str)>) {
    match filter {
        Filter::And(all) | Filter::Or(all) => all.iter().for_each(|f| texts(f, out)),
        Filter::Not(inner) => texts(inner, out),
        Filter::Text(field, value) => out.push((field, value)),
        _ => {}
    }
}

fn raw_matches(raw: &[u8], field: &TextField, value: &str) -> bool {
    let fields = mime::top_fields(raw);
    let joined = |name: &str| {
        fields
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let text = match field {
        TextField::Any => String::from_utf8_lossy(raw).into_owned(),
        TextField::Body => {
            let end = mime::header_end(raw, 0, raw.len());
            String::from_utf8_lossy(&raw[end..]).into_owned()
        }
        TextField::Subject => joined("subject"),
        TextField::From => joined("from"),
        TextField::To => joined("to"),
        TextField::Cc => joined("cc"),
        TextField::Bcc => joined("bcc"),
        TextField::Header(name) => {
            let name = name.to_ascii_lowercase();
            if value.trim().is_empty() {
                return fields.iter().any(|(n, _)| *n == name);
            }
            joined(&name)
        }
    };
    let have = mime::words(&text);
    mime::words(value)
        .iter()
        .all(|w| have.iter().any(|h| h.starts_with(w.as_str())))
}

impl Lookup {
    fn matches(&self, member: &Member, filter: &Filter) -> bool {
        let facts = self.facts.get(&member.email);
        match filter {
            Filter::All => true,
            Filter::And(all) => all.iter().all(|f| self.matches(member, f)),
            Filter::Or(any) => any.iter().any(|f| self.matches(member, f)),
            Filter::Not(inner) => !self.matches(member, inner),
            Filter::Text(field, value) => match (member.email, clause(field, value)) {
                (EmailRef::Message(id), Some(_)) => self
                    .text
                    .get(&text_key(field, value))
                    .is_some_and(|set| set.contains(&id)),
                _ => self
                    .raw
                    .get(&member.email)
                    .is_some_and(|raw| raw_matches(raw, field, value)),
            },
            Filter::Flag(flag) => self
                .flags
                .get(&member.email)
                .is_some_and(|f| f.iter().any(|f| same(f, flag))),
            Filter::ReceivedBefore(t) => facts.is_some_and(|f| day(f.received_at) < day(*t)),
            Filter::ReceivedSince(t) => facts.is_some_and(|f| day(f.received_at) >= day(*t)),
            Filter::SentBefore(t) => {
                facts.is_some_and(|f| f.sent_at.is_some_and(|s| day(s) < day(*t)))
            }
            Filter::SentSince(t) => {
                facts.is_some_and(|f| f.sent_at.is_some_and(|s| day(s) >= day(*t)))
            }
            Filter::Larger(n) => facts.is_some_and(|f| f.size > *n),
            Filter::Smaller(n) => facts.is_some_and(|f| f.size < *n),
            Filter::Uids(ranges) => ranges
                .iter()
                .any(|(a, b)| (*a.min(b)..=*a.max(b)).contains(&member.uid)),
            Filter::ModseqSince(m) => member.modseq >= *m,
            Filter::HasAttachment => facts.is_some_and(|f| f.attachment),
        }
    }
}

impl ArchiveMailView {
    /// The members that match `filter`, in the order given.
    pub(crate) async fn matching(
        &self,
        ctx: &Ctx,
        entry: &Entry,
        members: Vec<Member>,
        filter: &Filter,
    ) -> Result<Vec<Member>, MailViewError> {
        let emails: Vec<EmailRef> = members.iter().map(|m| m.email).collect();
        let ids: Vec<MessageId> = emails.iter().filter_map(EmailRef::message).collect();
        let mut lookup = Lookup {
            facts: HashMap::new(),
            flags: self.effective(ctx, entry, &emails).await?,
            raw: HashMap::new(),
            text: HashMap::new(),
        };
        let mut dates: Option<(Timestamp, Timestamp)> = None;
        for message in self.messages(ctx, &ids).await? {
            dates = Some(match dates {
                None => (message.date, message.date),
                Some((lo, hi)) => (lo.min(message.date), hi.max(message.date)),
            });
            lookup.facts.insert(
                EmailRef::Message(message.id),
                Facts {
                    size: message.size,
                    received_at: message.received_at.unwrap_or(message.archived_at),
                    sent_at: message.sent_at,
                    attachment: message.attachment_count > 0,
                },
            );
        }
        let raw_everywhere = needs_raw(filter);
        for email in &emails {
            if raw_everywhere || email.message().is_none() {
                let Some((raw, received_at, _)) = self.bytes(ctx, email).await? else {
                    continue;
                };
                if email.message().is_none() {
                    let fields = mime::top_fields(&raw);
                    let parsed = holtmail_mime::parse(&raw).ok();
                    lookup.facts.insert(
                        *email,
                        Facts {
                            size: raw.len() as u64,
                            received_at,
                            sent_at: parsed.and_then(|p| p.sent_at).map(Timestamp::from_secs),
                            attachment: mime::has_attachment(&mime::structure(&raw))
                                || mime::get(&fields, "content-disposition")
                                    .is_some_and(|d| d.starts_with("attachment")),
                        },
                    );
                }
                lookup.raw.insert(*email, raw);
            }
        }
        let mut wanted = Vec::new();
        texts(filter, &mut wanted);
        for (field, value) in wanted {
            let Some(clause) = clause(field, value) else {
                continue;
            };
            let key = text_key(field, value);
            if lookup.text.contains_key(&key) || ids.is_empty() {
                continue;
            }
            let mut clauses = vec![clause, Clause::Kind(DocumentKind::Mail)];
            if let Some((lo, hi)) = dates {
                clauses.push(Clause::After(lo));
                clauses.push(Clause::Before(Timestamp::from_millis(hi.as_millis() + 1)));
            }
            let cap = self.config.search_cap.min(MAX_MATCHES);
            let found = self
                .index
                .matching(ctx.viewer.archive, &SearchQuery { clauses }, cap)
                .await
                .map_err(unavailable)?;
            if found.is_cut() {
                return Err(MailViewError::TooMany {
                    limit: u64::from(cap),
                });
            }
            lookup.text.insert(key, found.ids.into_iter().collect());
        }
        Ok(members
            .into_iter()
            .filter(|m| lookup.matches(m, filter))
            .collect())
    }
}
