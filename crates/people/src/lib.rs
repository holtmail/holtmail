//! People derived from an archive's contacts and mail.
//!
//! Contact cards from every address book of an archive are grouped into people by a shared email
//! address or phone number, never by name; addresses of the mail join the person whose card has
//! them, and frequent correspondents without a card become people from mail. Splits and joins
//! made by hand ([`holtmail_store::PersonOverride`]) are the only stored state; everything else
//! is derived: [`rebuild`] replaces an archive's people from the stored contacts, messages and
//! overrides, and gives the same people with the same ids every time the data is the same.
//!
//! A library, not a contract: there is one grouping. Nothing here logs.

mod fields;
mod group;

use holtmail_core::ArchiveId;
use holtmail_pim::Normalized;
use holtmail_store::{
    ItemKind, ItemOrder, ItemQuery, ItemSummary, Person, PersonOverride, RemovedFilter, Store,
    StoreError,
};

pub use fields::{Field, FieldValue, SourceCard, contact_name, fields};
pub use group::{Card, MIN_MESSAGES, anchor, group, holds, is_correspondent, person_id};

/// What a rebuild made.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rebuilt {
    /// People in all.
    pub people: u64,
    /// People with at least one card.
    pub with_cards: u64,
    /// People from mail alone.
    pub from_mail: u64,
}

impl std::ops::AddAssign for Rebuilt {
    fn add_assign(&mut self, other: Self) {
        self.people += other.people;
        self.with_cards += other.with_cards;
        self.from_mail += other.from_mail;
    }
}

/// A stored contact as a [`Card`]: its latest normalized fields, `None` when they could not be
/// read or were never built.
pub fn card(summary: &ItemSummary) -> Card {
    let contact = summary
        .index
        .as_ref()
        .and_then(|i| serde_json::from_value::<Normalized>(i.data.clone()).ok())
        .and_then(|n| match n {
            Normalized::Contact(contact) => Some(*contact),
            _ => None,
        });
    Card {
        item_id: summary.item.id,
        collection_id: summary.item.collection_id,
        removed: summary.item.removed_at.is_some(),
        contact,
    }
}

/// Every contact of the archive, removed ones too, as cards.
pub async fn cards(store: &dyn Store, archive: ArchiveId) -> Result<Vec<Card>, StoreError> {
    let mut out = Vec::new();
    let mut after = None;
    loop {
        let page = store
            .list_items(&ItemQuery {
                archive_id: archive,
                kind: Some(ItemKind::Contact),
                collection_id: None,
                removed: RemovedFilter::Include,
                order: ItemOrder::Title,
                after,
                limit: 500,
            })
            .await?;
        out.extend(page.items.iter().map(card));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    Ok(out)
}

/// The people the archive's contacts and mail give with these splits and joins, without
/// storing them.
pub async fn preview(
    store: &dyn Store,
    archive: ArchiveId,
    overrides: &[PersonOverride],
) -> Result<Vec<Person>, StoreError> {
    let cards = cards(store, archive).await?;
    let mail = store.mail_addresses(archive).await?;
    Ok(group(archive, &cards, &mail, overrides))
}

/// Replaces the archive's people with the ones its contacts, mail and stored splits and joins
/// give now. Idempotent; the same contacts, mail and overrides give the same people with the
/// same ids.
pub async fn rebuild(store: &dyn Store, archive: ArchiveId) -> Result<Rebuilt, StoreError> {
    let overrides = store.person_overrides(archive).await?;
    let people = preview(store, archive, &overrides).await?;
    store.replace_people(archive, &people).await?;
    let with_cards = people.iter().filter(|p| !p.cards.is_empty()).count() as u64;
    Ok(Rebuilt {
        people: people.len() as u64,
        with_cards,
        from_mail: people.len() as u64 - with_cards,
    })
}

/// [`rebuild`] for every archive, summed.
pub async fn rebuild_all(store: &dyn Store) -> Result<Rebuilt, StoreError> {
    let mut total = Rebuilt::default();
    for archive in store.list_archives().await? {
        total += rebuild(store, archive.id).await?;
    }
    Ok(total)
}
