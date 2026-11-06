use holtmail_core::ItemId;
use holtmail_pim::Contact;
use serde::{Deserialize, Serialize};

/// A contact card as one source of a person's fields.
#[derive(Debug, Clone, Copy)]
pub struct SourceCard<'a> {
    /// The card.
    pub item_id: ItemId,
    /// Whether it was marked removed from its source.
    pub removed: bool,
    /// What it says.
    pub contact: &'a Contact,
}

/// One value of a field and the cards that say it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldValue {
    /// The value as the first card that says it writes it.
    pub value: String,
    /// The cards that say it, in the order they were given.
    pub sources: Vec<ItemId>,
}

/// A field of a person merged from their cards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    /// `name`, `nickname`, `email`, `phone`, `organization`, `title`, `birthday`, `address` or
    /// `note`.
    pub name: String,
    /// Every value any card says, each once, in the order the cards say them.
    pub values: Vec<FieldValue>,
    /// Whether the cards not marked removed that fill the field disagree on it. Nicknames and
    /// notes never count as disagreeing.
    pub differs: bool,
}

const COMPARED: [&str; 7] = [
    "name",
    "email",
    "phone",
    "organization",
    "title",
    "birthday",
    "address",
];

fn plain(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A contact's own name: `FN`, else the components of `N`; never the organization or an
/// address.
pub fn contact_name(contact: &Contact) -> Option<String> {
    contact
        .full_name
        .clone()
        .or_else(|| contact.name.as_ref().map(|n| n.formatted()))
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
}

fn values(field: &str, contact: &Contact) -> Vec<(String, String)> {
    let single = |value: Option<&String>| -> Vec<(String, String)> {
        value
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(|v| vec![(plain(v), v.to_owned())])
            .unwrap_or_default()
    };
    let mut out: Vec<(String, String)> = match field {
        "name" => single(contact_name(contact).as_ref()),
        "nickname" => contact
            .nicknames
            .iter()
            .flat_map(|n| single(Some(n)))
            .collect(),
        "email" => contact
            .emails
            .iter()
            .map(|e| {
                (
                    e.normalized
                        .clone()
                        .unwrap_or_else(|| e.address.trim().to_lowercase()),
                    e.address.trim().to_owned(),
                )
            })
            .collect(),
        "phone" => contact
            .phones
            .iter()
            .map(|p| {
                (
                    p.e164
                        .clone()
                        .unwrap_or_else(|| p.number.chars().filter(char::is_ascii_digit).collect()),
                    p.number.trim().to_owned(),
                )
            })
            .collect(),
        "organization" => single(contact.organization.as_ref()),
        "title" => single(contact.title.as_ref()),
        "birthday" => single(contact.birthday.as_ref()),
        "address" => contact
            .addresses
            .iter()
            .map(|a| a.formatted())
            .flat_map(|a| single(Some(&a)))
            .collect(),
        "note" => single(contact.note.as_ref()),
        _ => Vec::new(),
    };
    out.retain(|(key, _)| !key.is_empty());
    let mut seen = std::collections::HashSet::new();
    out.retain(|(key, _)| seen.insert(key.clone()));
    out
}

/// Merges the cards' fields: every value with the cards that say it, and whether the cards
/// disagree. Two values are the same when they normalize alike: emails as in
/// [`holtmail_pim::normalize_email`], phones by their E.164 form (else their digits), other
/// text without regard to letter case and runs of spaces. Cards that leave a field empty do not
/// disagree with the others; removed cards still give values but never disagree. Fields no card
/// fills are left out.
pub fn fields(cards: &[SourceCard<'_>]) -> Vec<Field> {
    let mut out = Vec::new();
    for name in [
        "name",
        "nickname",
        "email",
        "phone",
        "organization",
        "title",
        "birthday",
        "address",
        "note",
    ] {
        let mut merged: Vec<(String, FieldValue)> = Vec::new();
        let mut live_sets: Vec<Vec<String>> = Vec::new();
        for card in cards {
            let said = values(name, card.contact);
            if !card.removed && !said.is_empty() {
                let mut keys: Vec<String> = said.iter().map(|(k, _)| k.clone()).collect();
                keys.sort();
                live_sets.push(keys);
            }
            for (key, value) in said {
                match merged.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, entry)) => {
                        if !entry.sources.contains(&card.item_id) {
                            entry.sources.push(card.item_id);
                        }
                    }
                    None => merged.push((
                        key,
                        FieldValue {
                            value,
                            sources: vec![card.item_id],
                        },
                    )),
                }
            }
        }
        if merged.is_empty() {
            continue;
        }
        let differs = COMPARED.contains(&name) && live_sets.windows(2).any(|w| w[0] != w[1]);
        out.push(Field {
            name: name.to_owned(),
            values: merged.into_iter().map(|(_, v)| v).collect(),
            differs,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use holtmail_pim::{Email, Phone};

    use super::*;

    fn card(name: &str, emails: &[&str], phones: &[&str]) -> Contact {
        Contact {
            full_name: Some(name.into()),
            emails: emails
                .iter()
                .map(|e| Email {
                    address: (*e).into(),
                    normalized: holtmail_pim::normalize_email(e),
                    kinds: Vec::new(),
                })
                .collect(),
            phones: phones
                .iter()
                .map(|p| Phone {
                    number: (*p).into(),
                    e164: holtmail_pim::normalize_phone(p),
                    kinds: Vec::new(),
                })
                .collect(),
            ..Contact::default()
        }
    }

    fn field<'a>(fields: &'a [Field], name: &str) -> &'a Field {
        fields.iter().find(|f| f.name == name).unwrap()
    }

    #[test]
    fn agreeing_values_show_once_with_every_source() {
        let (a, b) = (ItemId::new(), ItemId::new());
        let work = card("Ada Lovelace", &["Ada@Example.com"], &["+1 555 0100"]);
        let home = card(" ada  lovelace", &["ada@example.com"], &["+1 (555) 0100"]);
        let merged = fields(&[
            SourceCard {
                item_id: a,
                removed: false,
                contact: &work,
            },
            SourceCard {
                item_id: b,
                removed: false,
                contact: &home,
            },
        ]);
        for name in ["name", "email", "phone"] {
            let f = field(&merged, name);
            assert_eq!(f.values.len(), 1, "{name}");
            assert_eq!(f.values[0].sources, vec![a, b]);
            assert!(!f.differs, "{name}");
        }
        assert_eq!(field(&merged, "email").values[0].value, "Ada@Example.com");
        assert!(merged.iter().all(|f| f.name != "title"));
    }

    #[test]
    fn disagreeing_values_are_flagged_unless_a_card_is_silent_or_removed() {
        let (a, b, c) = (ItemId::new(), ItemId::new(), ItemId::new());
        let work = card("Ada", &["ada@example.com"], &["+1 555 0100"]);
        let mut home = card("Ada", &["ada@example.com"], &["+1 555 0199"]);
        home.title = Some("Analyst".into());
        let old = card("Ada", &["ada@example.com"], &["+1 555 0142"]);
        let merged = fields(&[
            SourceCard {
                item_id: a,
                removed: false,
                contact: &work,
            },
            SourceCard {
                item_id: b,
                removed: false,
                contact: &home,
            },
            SourceCard {
                item_id: c,
                removed: true,
                contact: &old,
            },
        ]);
        let phone = field(&merged, "phone");
        assert!(phone.differs);
        assert_eq!(phone.values.len(), 3);
        assert_eq!(phone.values[2].sources, vec![c]);
        assert!(!field(&merged, "title").differs, "one card says nothing");
        assert!(!field(&merged, "email").differs);

        let merged = fields(&[
            SourceCard {
                item_id: a,
                removed: false,
                contact: &work,
            },
            SourceCard {
                item_id: c,
                removed: true,
                contact: &old,
            },
        ]);
        assert!(
            !field(&merged, "phone").differs,
            "removed cards never disagree"
        );
    }

    #[test]
    fn notes_never_disagree() {
        let mut one = card("Ada", &[], &[]);
        one.note = Some("Met at the conference".into());
        let mut two = card("Ada", &[], &[]);
        two.note = Some("Prefers mornings".into());
        let merged = fields(&[
            SourceCard {
                item_id: ItemId::new(),
                removed: false,
                contact: &one,
            },
            SourceCard {
                item_id: ItemId::new(),
                removed: false,
                contact: &two,
            },
        ]);
        let note = field(&merged, "note");
        assert_eq!(note.values.len(), 2);
        assert!(!note.differs);
    }
}
