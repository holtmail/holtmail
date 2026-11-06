use std::collections::{BTreeMap, HashMap, HashSet};

use holtmail_core::{ArchiveId, CollectionId, ItemId, PersonId};
use holtmail_pim::Contact;
use holtmail_store::{
    MailAddress, MailAddresses, Person, PersonCard, PersonLink, PersonMail, PersonOverride,
    PersonOverrideKind,
};

use crate::fields::{SourceCard, contact_name, fields};

/// A contact card to group.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    /// The contact.
    pub item_id: ItemId,
    /// Its address book.
    pub collection_id: CollectionId,
    /// Whether it was marked removed from its source.
    pub removed: bool,
    /// What its latest version says; `None` when it could not be read.
    pub contact: Option<Contact>,
}

/// How many messages make an address without a card a person, unless the owner wrote to it.
pub const MIN_MESSAGES: u64 = 2;

const AUTOMATED: [&str; 9] = [
    "noreply",
    "no-reply",
    "no_reply",
    "donotreply",
    "do-not-reply",
    "do_not_reply",
    "mailer-daemon",
    "postmaster",
    "bounce",
];

/// Whether an address without a card is a person from mail: the owner wrote to it, or it is in
/// at least [`MIN_MESSAGES`] messages and its local part does not start like an automatic
/// sender's (`noreply`, `mailer-daemon`, `bounce`, …).
pub fn is_correspondent(address: &MailAddress) -> bool {
    if address.from_me > 0 {
        return true;
    }
    let local = address.email.split('@').next().unwrap_or_default();
    address.messages >= MIN_MESSAGES && !is_automated_local(local)
}

/// Whether a local part (the part of an address before `@`) starts like an automatic sender's:
/// `noreply`, `no-reply`, `no_reply`, `donotreply`, `do-not-reply`, `do_not_reply`,
/// `mailer-daemon`, `postmaster` or `bounce`, without regard to ASCII letter case. Anything
/// after the prefix is allowed (`noreply-billing`, `bounces+123`).
pub fn is_automated_local(local: &str) -> bool {
    AUTOMATED.iter().any(|a| {
        local
            .get(..a.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(a))
    })
}

/// The id of the person whose smallest key is `key`: a UUIDv8 from the BLAKE3 hash of the
/// archive id and the key, so the same group gets the same id in every rebuild.
pub fn person_id(archive: ArchiveId, key: &str) -> PersonId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(archive.to_string().as_bytes());
    hasher.update(b"\n");
    hasher.update(key.as_bytes());
    let hash = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    PersonId::from_hash(bytes)
}

fn card_keys(contact: Option<&Contact>) -> Vec<String> {
    let Some(contact) = contact else {
        return Vec::new();
    };
    let mut keys: Vec<String> = contact
        .emails
        .iter()
        .filter_map(|e| e.normalized.as_ref())
        .map(|e| format!("email:{e}"))
        .chain(
            contact
                .phones
                .iter()
                .filter_map(|p| p.e164.as_ref())
                .map(|p| format!("tel:{p}")),
        )
        .collect();
    let mut seen = HashSet::new();
    keys.retain(|k| seen.insert(k.clone()));
    keys
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

fn most_common(names: impl Iterator<Item = String>) -> Option<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for name in names {
        match counts.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count += 1,
            None => counts.push((name, 1)),
        }
    }
    let most = counts.iter().map(|(_, c)| *c).max()?;
    counts.into_iter().find(|(_, c)| *c == most).map(|(n, _)| n)
}

fn sum(addresses: &[&MailAddress]) -> Option<PersonMail> {
    let first = addresses.first()?;
    let mut mail = PersonMail {
        messages: 0,
        from_them: 0,
        from_me: 0,
        first_at: first.first_at,
        last_at: first.last_at,
    };
    for address in addresses {
        mail.messages += address.messages;
        mail.from_them += address.from_them;
        mail.from_me += address.from_me;
        mail.first_at = mail.first_at.min(address.first_at);
        mail.last_at = mail.last_at.max(address.last_at);
    }
    Some(mail)
}

struct Group<'a> {
    cards: Vec<&'a Card>,
    keys: Vec<String>,
    links: Vec<PersonLink>,
    mail: Vec<&'a MailAddress>,
    card_emails: Vec<String>,
    moved: bool,
}

/// The key a join made by hand names a person by: its first card by item id as
/// `item:<item id>`, else its address as `email:<address>` for a person from mail.
pub fn anchor(person: &Person) -> Option<String> {
    match person.cards.iter().map(|c| c.item_id).min() {
        Some(item) => Some(format!("item:{item}")),
        None => person
            .email
            .as_ref()
            .map(|e| format!("email:{}", e.to_lowercase())),
    }
}

/// Whether the person holds `key`: one of its cards for `item:<id>`, one of its addresses for
/// `email:<address>`, one of its phones for `tel:<E.164>`.
pub fn holds(person: &Person, key: &str) -> bool {
    match key.split_once(':') {
        Some(("item", id)) => person.cards.iter().any(|c| c.item_id.to_string() == id),
        Some(("email", email)) => person.emails.iter().any(|e| e.eq_ignore_ascii_case(email)),
        Some(("tel", phone)) => person.phones.iter().any(|p| p == phone),
        _ => false,
    }
}

struct Sets {
    parent: Vec<usize>,
    sides: Vec<Vec<(usize, bool)>>,
    moved: Vec<bool>,
}

impl Sets {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            sides: vec![Vec::new(); n],
            moved: vec![false; n],
        }
    }

    fn find(&mut self, i: usize) -> usize {
        find(&mut self.parent, i)
    }

    fn apart(&mut self, a: usize, b: usize) -> bool {
        let (a, b) = (self.find(a), self.find(b));
        a != b
            && self.sides[a]
                .iter()
                .any(|(split, side)| self.sides[b].contains(&(*split, !side)))
    }

    fn union(&mut self, a: usize, b: usize) -> bool {
        if self.apart(a, b) {
            return false;
        }
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            let (root, child) = (a.min(b), a.max(b));
            self.parent[child] = root;
            let sides = std::mem::take(&mut self.sides[child]);
            for side in sides {
                if !self.sides[root].contains(&side) {
                    self.sides[root].push(side);
                }
            }
            self.moved[root] |= self.moved[child];
        }
        true
    }
}

/// Groups the archive's contact cards into people and adds the people from mail.
///
/// Cards that share a normalized email address or an E.164 phone number are the same person,
/// transitively; a name alone never joins cards. Removed cards are grouped like the others. An
/// address of the mail joins the person with a card that has it (compared without regard to
/// letter case); an address without a card is a person of its own when
/// [`is_correspondent`] says so. The owner's addresses are never people from mail and never
/// count as anyone's mail. A person's id comes from [`person_id`] with the group's smallest key
/// (`email:…`, `tel:…`, or `item:<id>` for a card with neither). People come out sorted by id.
///
/// `overrides` are the splits and joins made by hand, applied in this order:
///
/// 1. A split always holds: its moved cards are never in one person with its `from` cards,
///    whatever they share. Its moved cards are one person together.
/// 2. A join makes its keys one person (an `email:` key without a card brings that address's
///    mail), unless that would put the two sides of a split together; then it is ignored.
/// 3. Shared keys join cards as above, never across a split. Cards that arrive later and share
///    keys with both sides of a split stay with the side that was not moved out; they join the
///    moved cards only through a key no card of the other side has.
///
/// Keys of cards that no longer exist are ignored. When both sides of a split keep a key, the
/// side that was not moved out keeps the person's id and the mail of the shared addresses; the
/// moved side takes its smallest key no other person took first, else `item:<its first card>`.
pub fn group(
    archive: ArchiveId,
    cards: &[Card],
    mail: &MailAddresses,
    overrides: &[PersonOverride],
) -> Vec<Person> {
    let mut cards: Vec<&Card> = cards.iter().collect();
    cards.sort_by_key(|c| c.item_id);
    let mut keys: Vec<Vec<String>> = cards
        .iter()
        .map(|c| card_keys(c.contact.as_ref()))
        .collect();
    let by_item: HashMap<String, usize> = cards
        .iter()
        .enumerate()
        .map(|(i, c)| (format!("item:{}", c.item_id), i))
        .collect();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for (i, card_keys) in keys.iter().enumerate() {
        for key in card_keys {
            by_key.entry(key.clone()).or_insert(i);
        }
    }
    let mut virtual_keys: Vec<String> = Vec::new();
    let mut overrides: Vec<&PersonOverride> = overrides
        .iter()
        .filter(|o| o.archive_id == archive)
        .collect();
    overrides.sort_by_key(|o| o.id);
    for o in overrides
        .iter()
        .filter(|o| o.kind == PersonOverrideKind::Join)
    {
        for key in &o.keys {
            let known = by_item.contains_key(key) || by_key.contains_key(key);
            let usable = key.starts_with("email:") || key.starts_with("tel:");
            if !known && usable && !virtual_keys.contains(key) {
                virtual_keys.push(key.clone());
            }
        }
    }
    let node = |key: &str, virtual_keys: &[String]| -> Option<usize> {
        by_item
            .get(key)
            .or_else(|| by_key.get(key))
            .copied()
            .or_else(|| {
                virtual_keys
                    .iter()
                    .position(|k| k == key)
                    .map(|at| cards.len() + at)
            })
    };
    let nodes = cards.len() + virtual_keys.len();
    let mut sets = Sets::new(nodes);
    let mut splits: Vec<(Vec<usize>, Vec<usize>)> = Vec::new();
    for o in overrides
        .iter()
        .filter(|o| o.kind == PersonOverrideKind::Split)
    {
        let moved: Vec<usize> = o
            .keys
            .iter()
            .filter_map(|k| node(k, &virtual_keys))
            .collect();
        let rest: Vec<usize> = o
            .from
            .iter()
            .filter_map(|k| node(k, &virtual_keys))
            .filter(|n| !moved.contains(n))
            .collect();
        if moved.is_empty() || rest.is_empty() {
            continue;
        }
        let at = splits.len();
        for &n in &moved {
            sets.sides[n].push((at, true));
            sets.moved[n] = true;
        }
        for &n in &rest {
            if !sets.sides[n].contains(&(at, false)) {
                sets.sides[n].push((at, false));
            }
        }
        splits.push((moved, rest));
    }
    for (moved, _) in &splits {
        for pair in moved.windows(2) {
            sets.union(pair[0], pair[1]);
        }
    }
    let mut joined: Vec<(usize, usize, String)> = Vec::new();
    for o in overrides
        .iter()
        .filter(|o| o.kind == PersonOverrideKind::Join)
    {
        let members: Vec<usize> = o
            .keys
            .iter()
            .filter_map(|k| node(k, &virtual_keys))
            .collect();
        let together = members
            .iter()
            .all(|&m| !members.iter().any(|&n| sets.apart(m, n)));
        if !together {
            continue;
        }
        for pair in members.windows(2) {
            sets.union(pair[0], pair[1]);
            if pair[0] < cards.len() && pair[1] < cards.len() && pair[0] != pair[1] {
                joined.push((pair[0], pair[1], format!("hand:{}", o.id)));
            }
        }
    }
    for pass in [false, true] {
        let mut first_with: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, card_keys) in keys.iter().enumerate() {
            for key in card_keys {
                let earlier = first_with.entry(key.as_str()).or_default();
                for &other in earlier.iter() {
                    let (a, b) = (sets.find(other), sets.find(i));
                    if !pass && (sets.moved[a] || sets.moved[b]) {
                        continue;
                    }
                    if sets.union(other, i) {
                        break;
                    }
                }
                earlier.push(i);
            }
        }
    }
    keys.extend(virtual_keys.iter().map(|k| vec![k.clone()]));

    let mut groups: BTreeMap<usize, Group<'_>> = BTreeMap::new();
    for (i, node_keys) in keys.iter().enumerate() {
        let root = sets.find(i);
        let group = groups.entry(root).or_insert_with(|| Group {
            cards: Vec::new(),
            keys: Vec::new(),
            links: Vec::new(),
            mail: Vec::new(),
            card_emails: Vec::new(),
            moved: sets.moved[root],
        });
        for key in node_keys {
            group.keys.push(key.clone());
            if let Some(email) = key.strip_prefix("email:") {
                group.card_emails.push(email.to_owned());
            }
        }
        if let Some(card) = cards.get(i) {
            group.cards.push(card);
            if node_keys.is_empty() {
                group.keys.push(format!("item:{}", card.item_id));
            }
        }
    }
    let mut first_in: HashMap<(&str, usize), usize> = HashMap::new();
    for (i, card_keys) in keys.iter().enumerate().take(cards.len()) {
        let root = sets.find(i);
        for key in card_keys {
            match first_in.get(&(key.as_str(), root)) {
                Some(&first) => {
                    if let Some(group) = groups.get_mut(&root) {
                        group.links.push(PersonLink {
                            from: cards[first].item_id,
                            to: cards[i].item_id,
                            key: key.clone(),
                        });
                    }
                }
                None => {
                    first_in.insert((key.as_str(), root), i);
                }
            }
        }
    }
    for (from, to, key) in joined {
        let root = sets.find(from);
        if let Some(group) = groups.get_mut(&root)
            && !group.links.iter().any(|l| {
                (l.from == cards[from].item_id && l.to == cards[to].item_id)
                    || (l.from == cards[to].item_id && l.to == cards[from].item_id)
            })
        {
            group.links.push(PersonLink {
                from: cards[from].item_id,
                to: cards[to].item_id,
                key,
            });
        }
    }

    let mut order: Vec<usize> = groups.keys().copied().collect();
    order.sort_by_key(|root| (groups[root].moved, *root));
    let own: HashSet<String> = mail.own.iter().map(|e| e.to_lowercase()).collect();
    let mut by_email: HashMap<String, usize> = HashMap::new();
    for root in &order {
        for email in &groups[root].card_emails {
            by_email.entry(email.clone()).or_insert(*root);
        }
    }
    let mut from_mail: BTreeMap<String, Vec<&MailAddress>> = BTreeMap::new();
    for address in &mail.addresses {
        let key = address.email.to_lowercase();
        if own.contains(&key) {
            continue;
        }
        match by_email.get(key.as_str()) {
            Some(root) => {
                if let Some(group) = groups.get_mut(root) {
                    group.mail.push(address);
                }
            }
            None => from_mail.entry(key).or_default().push(address),
        }
    }

    let mut claimed: HashSet<String> = HashSet::new();
    let mut people: Vec<Person> = Vec::new();
    for root in order {
        let Some(group) = groups.remove(&root) else {
            continue;
        };
        if group.cards.is_empty() && group.mail.is_empty() {
            continue;
        }
        let mut candidates: Vec<&String> = group.keys.iter().collect();
        candidates.sort();
        let key = candidates
            .into_iter()
            .find(|k| !claimed.contains(*k))
            .cloned()
            .or_else(|| group.cards.first().map(|c| format!("item:{}", c.item_id)))
            .unwrap_or_default();
        claimed.insert(key.clone());
        people.push(card_person(archive, group, &key, &own));
    }
    for (email, addresses) in from_mail {
        let Some(total) = sum(&addresses) else {
            continue;
        };
        let merged = MailAddress {
            email: email.clone(),
            name: None,
            messages: total.messages,
            from_them: total.from_them,
            from_me: total.from_me,
            first_at: total.first_at,
            last_at: total.last_at,
        };
        if !is_correspondent(&merged) {
            continue;
        }
        let mut emails: Vec<String> = addresses.iter().map(|a| a.email.clone()).collect();
        emails.sort();
        emails.dedup();
        people.push(Person {
            id: person_id(archive, &format!("email:{email}")),
            archive_id: archive,
            name: name_from_mail(&addresses),
            email: Some(email),
            phone: None,
            mail_emails: emails.clone(),
            emails,
            phones: Vec::new(),
            cards: Vec::new(),
            links: Vec::new(),
            mail: Some(total),
            differences: false,
            removed: false,
        });
    }
    people.sort_by_key(|p| p.id);
    people
}

fn name_from_mail(addresses: &[&MailAddress]) -> Option<String> {
    addresses
        .iter()
        .filter(|a| a.name.is_some())
        .max_by(|a, b| a.messages.cmp(&b.messages).then(b.email.cmp(&a.email)))
        .and_then(|a| a.name.clone())
}

fn card_person(archive: ArchiveId, group: Group<'_>, key: &str, own: &HashSet<String>) -> Person {
    let live: Vec<&Card> = group.cards.iter().copied().filter(|c| !c.removed).collect();
    let shown = if live.is_empty() { &group.cards } else { &live };
    let name = most_common(
        shown
            .iter()
            .filter_map(|c| c.contact.as_ref().and_then(contact_name)),
    )
    .or_else(|| name_from_mail(&group.mail));
    let email = shown
        .iter()
        .filter_map(|c| c.contact.as_ref())
        .flat_map(|c| c.emails.iter())
        .find_map(|e| e.normalized.clone())
        .or_else(|| {
            group
                .mail
                .iter()
                .max_by_key(|a| a.messages)
                .map(|a| a.email.clone())
        });
    let phone = shown
        .iter()
        .filter_map(|c| c.contact.as_ref())
        .flat_map(|c| c.phones.iter())
        .map(|p| p.e164.clone().unwrap_or_else(|| p.number.clone()))
        .next();
    let mut emails: Vec<String> = group
        .card_emails
        .iter()
        .cloned()
        .chain(group.mail.iter().map(|a| a.email.clone()))
        .collect();
    emails.sort();
    emails.dedup();
    let mail_emails: Vec<String> = emails
        .iter()
        .filter(|e| !own.contains(&e.to_lowercase()))
        .cloned()
        .collect();
    let mut phones: Vec<String> = group
        .keys
        .iter()
        .filter_map(|k| k.strip_prefix("tel:"))
        .map(str::to_owned)
        .collect();
    phones.sort();
    phones.dedup();
    let readable: Vec<SourceCard<'_>> = group
        .cards
        .iter()
        .filter_map(|c| {
            c.contact.as_ref().map(|contact| SourceCard {
                item_id: c.item_id,
                removed: c.removed,
                contact,
            })
        })
        .collect();
    let differences = fields(&readable).iter().any(|f| f.differs);
    let mail = sum(&group.mail);
    Person {
        id: person_id(archive, key),
        archive_id: archive,
        name,
        email,
        phone,
        emails,
        mail_emails,
        phones,
        cards: group
            .cards
            .iter()
            .map(|c| PersonCard {
                item_id: c.item_id,
                collection_id: c.collection_id,
                removed: c.removed,
            })
            .collect(),
        links: group.links,
        removed: live.is_empty() && mail.is_none(),
        mail,
        differences,
    }
}

#[cfg(test)]
mod tests {
    use holtmail_core::Timestamp;
    use holtmail_pim::{Email, Phone};

    use super::*;

    fn contact(name: &str, emails: &[&str], phones: &[&str]) -> Contact {
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

    fn card(collection: CollectionId, contact: Contact) -> Card {
        Card {
            item_id: ItemId::new(),
            collection_id: collection,
            removed: false,
            contact: Some(contact),
        }
    }

    fn address(email: &str, name: Option<&str>, messages: u64, from_me: u64) -> MailAddress {
        MailAddress {
            email: email.into(),
            name: name.map(Into::into),
            messages,
            from_them: messages - from_me,
            from_me,
            first_at: Timestamp::from_secs(1_700_000_000),
            last_at: Timestamp::from_secs(1_700_000_000 + 1000 * messages as i64),
        }
    }

    #[test]
    fn cards_join_through_shared_emails_and_phones_never_names() {
        let archive = ArchiveId::new();
        let (work, home) = (CollectionId::new(), CollectionId::new());
        let a = card(
            work,
            contact("Ada Lovelace", &["ada@example.com"], &["+1 555 0100"]),
        );
        let b = card(home, contact("Ada", &["ADA@example.com"], &["+1 555 0199"]));
        let c = card(home, contact("A. L.", &[], &["+1 555 0199"]));
        let d = card(work, contact("Ada Lovelace", &["other@example.org"], &[]));
        let e = card(work, contact("Nameless", &[], &[]));
        let people = group(
            archive,
            &[a.clone(), b.clone(), c.clone(), d.clone(), e.clone()],
            &MailAddresses::default(),
            &[],
        );
        assert_eq!(people.len(), 3, "{people:#?}");
        let ada = people.iter().find(|p| p.cards.len() == 3).unwrap();
        let ids: Vec<ItemId> = ada.cards.iter().map(|c| c.item_id).collect();
        assert_eq!(ids, vec![a.item_id, b.item_id, c.item_id]);
        assert_eq!(
            ada.links,
            vec![
                PersonLink {
                    from: a.item_id,
                    to: b.item_id,
                    key: "email:ada@example.com".into()
                },
                PersonLink {
                    from: b.item_id,
                    to: c.item_id,
                    key: "tel:+15550199".into()
                },
            ]
        );
        assert_eq!(ada.id, person_id(archive, "email:ada@example.com"));
        assert_eq!(ada.phones, vec!["+15550100", "+15550199"]);
        assert!(ada.differences, "the phones disagree");
        assert_eq!(ada.email.as_deref(), Some("ada@example.com"));
        assert!(!ada.removed);
        let alone = people
            .iter()
            .find(|p| p.cards[0].item_id == e.item_id)
            .unwrap();
        assert_eq!(alone.id, person_id(archive, &format!("item:{}", e.item_id)));
        assert_eq!(alone.name.as_deref(), Some("Nameless"));
    }

    #[test]
    fn ids_survive_a_rebuild_and_the_order_of_cards() {
        let archive = ArchiveId::new();
        let book = CollectionId::new();
        let cards = vec![
            card(book, contact("Ada", &["ada@example.com"], &[])),
            card(
                book,
                contact("Bob", &["bob@example.org"], &["+44 20 7946 0000"]),
            ),
        ];
        let mut reversed = cards.clone();
        reversed.reverse();
        let first = group(archive, &cards, &MailAddresses::default(), &[]);
        assert_eq!(
            first,
            group(archive, &reversed, &MailAddresses::default(), &[])
        );
        let other = group(ArchiveId::new(), &cards, &MailAddresses::default(), &[]);
        assert_ne!(first[0].id, other[0].id, "ids are per archive");
    }

    #[test]
    fn mail_joins_cards_and_makes_people_of_frequent_correspondents() {
        let archive = ArchiveId::new();
        let book = CollectionId::new();
        let ada = card(book, contact("Ada", &["ada@example.com"], &[]));
        let mail = MailAddresses {
            own: vec!["me@example.com".into()],
            addresses: vec![
                address("ada@example.com", Some("Ada L."), 3, 1),
                address("bob@example.org", Some("Bob Stone"), 2, 0),
                address("cleo@example.org", Some("Cleo"), 1, 1),
                address("dan@example.net", None, 1, 0),
                address("me@example.com", Some("Me"), 9, 0),
                address("noreply@example.com", Some("Shop"), 40, 0),
            ],
        };
        let people = group(archive, std::slice::from_ref(&ada), &mail, &[]);
        let names: Vec<Option<&str>> = people.iter().map(|p| p.name.as_deref()).collect();
        assert_eq!(people.len(), 3, "{names:?}");
        let with_card = people.iter().find(|p| !p.cards.is_empty()).unwrap();
        assert_eq!(with_card.name.as_deref(), Some("Ada"), "the card names her");
        assert_eq!(with_card.mail.unwrap().messages, 3);
        let bob = people
            .iter()
            .find(|p| p.email.as_deref() == Some("bob@example.org"))
            .unwrap();
        assert_eq!(bob.name.as_deref(), Some("Bob Stone"));
        assert_eq!(bob.id, person_id(archive, "email:bob@example.org"));
        assert!(
            people
                .iter()
                .any(|p| p.email.as_deref() == Some("cleo@example.org"))
        );

        let bob_card = card(book, contact("Robert", &["Bob@Example.org"], &[]));
        let again = group(archive, &[ada, bob_card], &mail, &[]);
        let bob_again = again
            .iter()
            .find(|p| p.emails.contains(&"bob@example.org".to_owned()))
            .unwrap();
        assert_eq!(
            bob_again.id, bob.id,
            "a new card keeps the person from mail"
        );
        assert_eq!(bob_again.name.as_deref(), Some("Robert"));
        assert_eq!(bob_again.mail.unwrap().messages, 2);
    }

    #[test]
    fn removed_cards_count_until_mail_is_gone_too() {
        let archive = ArchiveId::new();
        let book = CollectionId::new();
        let mut gone = card(book, contact("Old", &["old@example.com"], &[]));
        gone.removed = true;
        let mut unreadable = card(book, Contact::default());
        unreadable.contact = None;
        let people = group(
            archive,
            &[gone.clone(), unreadable],
            &MailAddresses::default(),
            &[],
        );
        let old = people
            .iter()
            .find(|p| p.cards[0].item_id == gone.item_id)
            .unwrap();
        assert!(old.removed);
        let mail = MailAddresses {
            own: Vec::new(),
            addresses: vec![address("old@example.com", None, 1, 0)],
        };
        let people = group(archive, std::slice::from_ref(&gone), &mail, &[]);
        assert!(!people[0].removed, "mail keeps them");
        assert_eq!(people[0].name.as_deref(), Some("Old"));
    }

    #[test]
    fn own_addresses_bring_no_mail() {
        let archive = ArchiveId::new();
        let book = CollectionId::new();
        let me = card(book, contact("Me", &["me@example.com"], &[]));
        let mail = MailAddresses {
            own: vec!["me@example.com".into()],
            addresses: vec![address("me@example.com", Some("Me"), 9, 0)],
        };
        let people = group(archive, &[me], &mail, &[]);
        assert_eq!(people[0].mail, None);
        assert_eq!(people[0].emails, vec!["me@example.com"]);
        assert!(people[0].mail_emails.is_empty());
    }

    fn join(archive: ArchiveId, keys: &[String]) -> PersonOverride {
        PersonOverride {
            id: holtmail_core::PersonOverrideId::new(),
            archive_id: archive,
            kind: PersonOverrideKind::Join,
            keys: keys.to_vec(),
            from: Vec::new(),
            created_at: Timestamp::from_secs(1_700_000_000),
            created_by: None,
        }
    }

    fn split(archive: ArchiveId, moved: &[&Card], rest: &[&Card]) -> PersonOverride {
        let item = |c: &&Card| format!("item:{}", c.item_id);
        PersonOverride {
            id: holtmail_core::PersonOverrideId::new(),
            archive_id: archive,
            kind: PersonOverrideKind::Split,
            keys: moved.iter().map(item).collect(),
            from: rest.iter().map(item).collect(),
            created_at: Timestamp::from_secs(1_700_000_000),
            created_by: None,
        }
    }

    fn holder<'a>(people: &'a [Person], card: &Card) -> &'a Person {
        people
            .iter()
            .find(|p| p.cards.iter().any(|c| c.item_id == card.item_id))
            .unwrap()
    }

    #[test]
    fn joins_by_hand_hold_without_a_shared_key() {
        let archive = ArchiveId::new();
        let book = CollectionId::new();
        let ada = card(book, contact("Ada", &["ada@example.com"], &[]));
        let lovelace = card(book, contact("A. Lovelace", &["al@example.org"], &[]));
        let mail = MailAddresses {
            own: Vec::new(),
            addresses: vec![address("countess@example.net", Some("Countess"), 3, 0)],
        };
        let cards = [ada.clone(), lovelace.clone()];
        let before = group(archive, &cards, &mail, &[]);
        assert_eq!(before.len(), 3);
        let from_mail = before.iter().find(|p| p.cards.is_empty()).unwrap();

        let both = join(
            archive,
            &[
                anchor(holder(&before, &ada)).unwrap(),
                anchor(holder(&before, &lovelace)).unwrap(),
            ],
        );
        let with_mail = join(
            archive,
            &[
                anchor(holder(&before, &ada)).unwrap(),
                anchor(from_mail).unwrap(),
            ],
        );
        assert_eq!(anchor(from_mail).unwrap(), "email:countess@example.net");
        let after = group(archive, &cards, &mail, &[both.clone(), with_mail]);
        assert_eq!(after.len(), 1, "{after:#?}");
        let person = &after[0];
        assert_eq!(person.cards.len(), 2);
        assert_eq!(person.mail.unwrap().messages, 3);
        assert!(person.emails.contains(&"countess@example.net".to_owned()));
        let first = ada.item_id.min(lovelace.item_id);
        let ids = [ada.item_id, lovelace.item_id];
        assert_eq!(
            person.links,
            vec![PersonLink {
                from: first,
                to: *ids.iter().find(|i| **i != first).unwrap(),
                key: format!("hand:{}", both.id),
            }]
        );
        assert_eq!(
            person.id,
            person_id(archive, "email:ada@example.com"),
            "the smallest key still names the person"
        );
        assert!(holds(person, &format!("item:{}", lovelace.item_id)));
        assert!(holds(person, "email:countess@example.net"));
        assert!(!holds(person, "email:nobody@example.net"));
    }

    #[test]
    fn splits_by_hand_hold_when_new_cards_arrive() {
        let archive = ArchiveId::new();
        let (work, home) = (CollectionId::new(), CollectionId::new());
        let a = card(work, contact("Ada", &["ada@example.com"], &["+1 555 0100"]));
        let b = card(home, contact("Ada", &["ada@example.com"], &[]));
        let c = card(
            home,
            contact("Ada Jr", &["ada@example.com"], &["+1 555 0300"]),
        );
        let mail = MailAddresses {
            own: Vec::new(),
            addresses: vec![address("ada@example.com", Some("Ada"), 4, 1)],
        };
        let cards = vec![a.clone(), b.clone(), c.clone()];
        let before = group(archive, &cards, &mail, &[]);
        assert_eq!(before.len(), 1);
        let original = before[0].id;

        let apart = split(archive, &[&c], &[&a, &b]);
        let after = group(archive, &cards, &mail, std::slice::from_ref(&apart));
        assert_eq!(after.len(), 2, "{after:#?}");
        let rest = holder(&after, &a);
        let moved = holder(&after, &c);
        assert_eq!(rest.id, original, "the cards left behind keep the id");
        assert_eq!(rest.cards.len(), 2);
        assert_eq!(
            rest.mail.unwrap().messages,
            4,
            "and the shared address's mail"
        );
        assert_ne!(moved.id, original);
        assert_eq!(moved.id, person_id(archive, "tel:+15550300"));
        assert_eq!(moved.mail, None);
        assert!(moved.links.is_empty());

        let late = card(work, contact("Ada", &["ada@example.com"], &[]));
        let junior = card(work, contact("Junior", &[], &["+1 555 0300"]));
        let mut more = cards.clone();
        more.extend([late.clone(), junior.clone()]);
        let again = group(archive, &more, &mail, std::slice::from_ref(&apart));
        assert_eq!(again.len(), 2, "{again:#?}");
        assert_eq!(
            holder(&again, &late).id,
            original,
            "shared with both: stays"
        );
        assert_eq!(
            holder(&again, &junior).id,
            moved.id,
            "only the moved side's key"
        );

        let bridge = join(
            archive,
            &[format!("item:{}", a.item_id), format!("item:{}", c.item_id)],
        );
        let ignored = group(archive, &cards, &mail, &[apart.clone(), bridge]);
        assert_eq!(ignored, after, "a join never crosses a split");

        assert_eq!(group(archive, &cards, &mail, &[]), before, "undo");
    }

    #[test]
    fn several_moved_cards_stay_together() {
        let archive = ArchiveId::new();
        let book = CollectionId::new();
        let a = card(book, contact("A", &["shared@example.com"], &[]));
        let b = card(
            book,
            contact("B", &["shared@example.com"], &["+1 555 0101"]),
        );
        let c = card(
            book,
            contact("C", &["shared@example.com"], &["+1 555 0102"]),
        );
        let cards = vec![a.clone(), b.clone(), c.clone()];
        let apart = split(archive, &[&b, &c], &[&a]);
        let people = group(archive, &cards, &MailAddresses::default(), &[apart]);
        assert_eq!(people.len(), 2);
        assert_eq!(holder(&people, &b).id, holder(&people, &c).id);
        assert_eq!(
            holder(&people, &b).links,
            vec![PersonLink {
                from: b.item_id,
                to: c.item_id,
                key: "email:shared@example.com".into()
            }]
        );
    }

    #[test]
    fn automated_local_parts() {
        for local in [
            "noreply",
            "no-reply-billing",
            "NoReply",
            "bounces+123",
            "MAILER-DAEMON",
            "postmaster",
            "do_not_reply",
        ] {
            assert!(is_automated_local(local), "{local}");
        }
        for local in ["ada", "reply", "info", "", "ñoreply", "no.reply"] {
            assert!(!is_automated_local(local), "{local}");
        }
    }
}
