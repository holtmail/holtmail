use std::collections::HashMap;

use bytes::Bytes;
use holtmail_blob::BlobError;
use holtmail_core::{CollectionId, PersonId, Timestamp};
use holtmail_pim::{Format, Method, Normalized};
use holtmail_store::{
    Archive, Collection, CollectionKind, ItemKind, ItemOrder, ItemQuery, ItemSummary, ItemVersion,
    RemovedFilter,
};
use serde_json::{Value, json};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::time::civil;
use crate::{ExportError, Exporter, names, zipped};

const PAGE: u32 = 500;

/// Contacts or calendar items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PimKind {
    /// Contacts of the archive's address books.
    Contacts,
    /// Events, tasks and other objects of the archive's calendars.
    Calendar,
}

/// How contacts or calendar items are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PimFormat {
    /// The latest version of each item in one `.vcf` (contacts) or `.ics` (calendar) file. A
    /// `.vcf` is the cards one after another, JSON sources through [`holtmail_pim::as_text`]. An
    /// `.ics` is one `VCALENDAR` through [`holtmail_pim::IcsWriter`]: iCalendar items contribute
    /// their components and zones unchanged, items from mail are written from their merged view
    /// (the latest full invitation, every attendee's last answer, `CANCELLED` once cancelled), JSON
    /// ones from their normalized view.
    Text,
    /// Every version of each item, its stored bytes unchanged, in a zip:
    /// `<collection>/<item>/<n> <seen date>.<vcf|ics|json>`, and `manifest.json` last.
    Zip,
}

impl PimFormat {
    /// `vcf`, `ics` or `text` for [`PimFormat::Text`] (`vcf` and `ics` only for their kind),
    /// `zip`, in any letter case; `None` otherwise.
    pub fn parse(raw: &str, kind: PimKind) -> Option<Self> {
        match (raw.trim().to_ascii_lowercase().as_str(), kind) {
            ("text", _) | ("vcf", PimKind::Contacts) | ("ics", PimKind::Calendar) => {
                Some(Self::Text)
            }
            ("zip", _) => Some(Self::Zip),
            _ => None,
        }
    }

    /// The file name extension, without the dot.
    pub fn extension(self, kind: PimKind) -> &'static str {
        match (self, kind) {
            (Self::Zip, _) => "zip",
            (Self::Text, PimKind::Contacts) => "vcf",
            (Self::Text, PimKind::Calendar) => "ics",
        }
    }

    /// The media type of the written file.
    pub fn content_type(self, kind: PimKind) -> &'static str {
        match (self, kind) {
            (Self::Zip, _) => "application/zip",
            (Self::Text, PimKind::Contacts) => "text/vcard; charset=utf-8",
            (Self::Text, PimKind::Calendar) => "text/calendar; charset=utf-8",
        }
    }
}

/// Which contacts or calendar items of an archive are exported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PimSelection {
    /// Contacts or calendar items.
    pub kind: PimKind,
    /// Only this address book or calendar.
    pub collection: Option<CollectionId>,
    /// Only this person's cards; contacts only.
    pub person: Option<PersonId>,
    /// Only events whose instances reach into `[from, to)`; calendar only. Tasks and objects
    /// without a start are left out then.
    pub range: Option<(Timestamp, Timestamp)>,
    /// Whether a [`PimFormat::Text`] export includes items removed from their source. A zip
    /// always has every item.
    pub removed: bool,
}

/// What a contacts or calendar export wrote so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PimTotals {
    /// Items written.
    pub items: u64,
    /// Versions written: one per item for a text export.
    pub versions: u64,
    /// Bytes written for them.
    pub bytes: u64,
    /// Versions left out: their bytes are missing or damaged, or a JSON version could not be
    /// read to be written as text.
    pub skipped: u64,
}

impl Exporter {
    /// Checks that the export can run and prepares it, before anything is written: the archive
    /// exists, the collection is one of its address books or calendars as `selection.kind` says,
    /// the person is one of its people, and the selection's parts fit its kind.
    pub async fn prepare_pim(
        &self,
        archive: holtmail_core::ArchiveId,
        selection: PimSelection,
        format: PimFormat,
    ) -> Result<PimExport, ExportError> {
        let archive = self
            .store
            .get_archive(archive)
            .await?
            .ok_or(ExportError::NotFound("archive"))?;
        let wanted = match selection.kind {
            PimKind::Contacts => CollectionKind::Contacts,
            PimKind::Calendar => CollectionKind::Calendar,
        };
        if selection.kind == PimKind::Contacts && selection.range.is_some() {
            return Err(ExportError::Invalid("a range is for calendars".into()));
        }
        if selection.kind == PimKind::Calendar && selection.person.is_some() {
            return Err(ExportError::Invalid("a person is for contacts".into()));
        }
        if let Some((from, to)) = selection.range
            && from >= to
        {
            return Err(ExportError::Invalid(
                "the range ends before it starts".into(),
            ));
        }
        let mut collections: Vec<Collection> = self
            .store
            .list_collections(archive.id)
            .await?
            .into_iter()
            .map(|c| c.collection)
            .filter(|c| c.kind == wanted)
            .collect();
        let mut scope = None;
        if let Some(id) = selection.collection {
            let found = collections
                .iter()
                .find(|c| c.id == id)
                .ok_or(ExportError::NotFound("collection"))?;
            scope = Some(found.name.clone());
            collections.retain(|c| c.id == id);
        }
        let mut cards = None;
        if let Some(id) = selection.person {
            let person = self
                .store
                .get_person(id)
                .await?
                .filter(|p| p.archive_id == archive.id)
                .ok_or(ExportError::NotFound("person"))?;
            scope = Some(person.name.clone().unwrap_or_else(|| "person".into()));
            cards = Some(person.cards.iter().map(|c| c.item_id).collect());
        }
        if scope.is_none() && selection.range.is_some() {
            scope = Some("range".into());
        }
        let mut directories = HashMap::new();
        let mut taken: Vec<String> = vec!["manifest.json".into()];
        let mut sorted = collections.clone();
        sorted.sort_by_key(|c| c.id);
        for collection in &sorted {
            let mut name = names::segment(&collection.name, 80);
            if taken.iter().any(|t| t.eq_ignore_ascii_case(&name)) {
                name = format!("{name} {}", collection.id);
            }
            taken.push(name.clone());
            directories.insert(collection.id, name);
        }
        Ok(PimExport {
            exporter: self.clone(),
            archive,
            selection,
            format,
            collections,
            cards,
            directories,
            scope,
        })
    }
}

/// One prepared contacts or calendar export.
pub struct PimExport {
    exporter: Exporter,
    archive: Archive,
    selection: PimSelection,
    format: PimFormat,
    collections: Vec<Collection>,
    cards: Option<Vec<holtmail_core::ItemId>>,
    directories: HashMap<CollectionId, String>,
    scope: Option<String>,
}

fn extension(content_type: &str) -> &'static str {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match essence.as_str() {
        "text/vcard" | "text/x-vcard" => "vcf",
        "text/calendar" => "ics",
        e if e.ends_with("+json") || e == "application/json" => "json",
        _ => "txt",
    }
}

fn seen(at: Timestamp) -> String {
    let c = civil(at);
    format!(
        "{:04}-{:02}-{:02} {:02}{:02}{:02}",
        c.year, c.month, c.day, c.hour, c.minute, c.second
    )
}

fn store_kind(kind: ItemKind) -> holtmail_pim::ItemKind {
    match kind {
        ItemKind::Contact => holtmail_pim::ItemKind::Contact,
        ItemKind::Event => holtmail_pim::ItemKind::Event,
        ItemKind::Task => holtmail_pim::ItemKind::Task,
        ItemKind::Other => holtmail_pim::ItemKind::Other,
    }
}

impl PimExport {
    /// A file name for the export: the archive's name, `contacts` or `calendar`, the collection,
    /// the person's name or `range` when narrowed, and the extension. Safe as a single path
    /// segment.
    pub fn file_name(&self) -> String {
        let what = match self.selection.kind {
            PimKind::Contacts => "contacts",
            PimKind::Calendar => "calendar",
        };
        let base = match &self.scope {
            Some(scope) => format!("{} - {what} - {scope}", self.archive.name),
            None => format!("{} - {what}", self.archive.name),
        };
        format!(
            "{}.{}",
            names::segment(&base, 120),
            self.format.extension(self.selection.kind)
        )
    }

    /// The format being written.
    pub fn format(&self) -> PimFormat {
        self.format
    }

    /// What is exported.
    pub fn kind(&self) -> PimKind {
        self.selection.kind
    }

    async fn items(&self) -> Result<Vec<ItemSummary>, ExportError> {
        let store = &self.exporter.store;
        let keep_removed = self.selection.removed || self.format == PimFormat::Zip;
        if let Some(cards) = &self.cards {
            let mut out = Vec::new();
            for id in cards {
                if let Some(summary) = store.get_item(*id).await?
                    && (keep_removed || summary.item.removed_at.is_none())
                    && self
                        .collections
                        .iter()
                        .any(|c| c.id == summary.item.collection_id)
                {
                    out.push(summary);
                }
            }
            return Ok(out);
        }
        let mut out = Vec::new();
        for collection in &self.collections {
            let mut after = None;
            loop {
                let page = store
                    .list_items(&ItemQuery {
                        archive_id: self.archive.id,
                        kind: (self.selection.kind == PimKind::Contacts)
                            .then_some(ItemKind::Contact),
                        collection_id: Some(collection.id),
                        removed: if keep_removed {
                            RemovedFilter::Include
                        } else {
                            RemovedFilter::Hide
                        },
                        order: ItemOrder::Title,
                        after,
                        limit: PAGE,
                    })
                    .await?;
                for summary in page.items {
                    if self.in_range(&summary).await? {
                        out.push(summary);
                    }
                }
                match page.next {
                    Some(next) => after = Some(next),
                    None => break,
                }
            }
        }
        Ok(out)
    }

    async fn in_range(&self, summary: &ItemSummary) -> Result<bool, ExportError> {
        let Some((from, to)) = self.selection.range else {
            return Ok(true);
        };
        let Some(series) = self.exporter.store.series(summary.item.id).await? else {
            return Ok(false);
        };
        Ok(match series.starts_at {
            Some(start) => start < to && series.ends_at.is_none_or(|end| end > from),
            None => false,
        })
    }

    async fn bytes(&self, version: &ItemVersion) -> Result<Option<Bytes>, ExportError> {
        match self.exporter.blobs.get(&version.blob_id).await {
            Ok(found) => Ok(found),
            Err(BlobError::Corrupt(_)) => Ok(None),
            Err(error) => Err(ExportError::Blob(error)),
        }
    }

    /// What a calendar item adds to the `.ics`: an iCalendar item's latest version as stored, an
    /// item from mail merged over all its versions as its page shows it, a JSON one from its
    /// normalized view. `None` when there is nothing readable to write.
    async fn calendar_text(
        &self,
        summary: &ItemSummary,
        writer: &mut holtmail_pim::IcsWriter,
    ) -> Result<Option<Vec<u8>>, ExportError> {
        let kind = store_kind(summary.item.kind);
        let versions = self.exporter.store.item_versions(summary.item.id).await?;
        let Some(latest) = versions.last() else {
            return Ok(None);
        };
        if versions.iter().any(|v| v.mail.is_some()) {
            let mut read = Vec::with_capacity(versions.len());
            for version in &versions {
                let normalized = match self.bytes(version).await? {
                    Some(bytes) => holtmail_pim::normalize_as(
                        Format::stored(&version.content_type, kind),
                        kind,
                        &bytes,
                    ),
                    None => Normalized::Unreadable,
                };
                let method = version.mail.as_ref().and_then(|m| m.method.as_deref());
                read.push((Method::of(method), normalized));
            }
            let pairs: Vec<_> = read.iter().map(|(m, n)| (*m, n)).collect();
            return Ok(match holtmail_pim::merge(&pairs) {
                Normalized::Calendar(object) => Some(writer.object(&object)),
                _ => None,
            });
        }
        let Some(bytes) = self.bytes(latest).await? else {
            return Ok(None);
        };
        Ok(match Format::stored(&latest.content_type, kind) {
            Format::ICalendar => writer.native(&bytes),
            format => match holtmail_pim::normalize_as(format, kind, &bytes) {
                Normalized::Calendar(object) => Some(writer.object(&object)),
                _ => None,
            },
        })
    }

    /// Writes the export to `out` and flushes it, calling `progress` after each item.
    ///
    /// Versions whose bytes are missing or corrupt, and JSON versions that cannot be read for a
    /// text export, are left out and counted in [`PimTotals::skipped`]; any other failure stops
    /// the export, leaving `out` incomplete.
    pub async fn write(
        self,
        out: &mut (dyn AsyncWrite + Send + Unpin),
        progress: &(dyn Fn(&PimTotals) + Send + Sync),
    ) -> Result<PimTotals, ExportError> {
        let items = self.items().await?;
        let mut totals = PimTotals::default();
        match self.format {
            PimFormat::Text if self.selection.kind == PimKind::Calendar => {
                let mut writer = holtmail_pim::IcsWriter::new();
                let head = writer.begin();
                out.write_all(&head).await?;
                totals.bytes += head.len() as u64;
                for summary in &items {
                    let Some(text) = self.calendar_text(summary, &mut writer).await? else {
                        totals.skipped += 1;
                        continue;
                    };
                    out.write_all(&text).await?;
                    totals.bytes += text.len() as u64;
                    totals.items += 1;
                    totals.versions += 1;
                    progress(&totals);
                }
                out.write_all(writer.end()).await?;
                totals.bytes += writer.end().len() as u64;
            }
            PimFormat::Text => {
                for summary in &items {
                    let versions = self.exporter.store.item_versions(summary.item.id).await?;
                    let Some(latest) = versions.last() else {
                        continue;
                    };
                    let Some(bytes) = self.bytes(latest).await? else {
                        totals.skipped += 1;
                        continue;
                    };
                    let kind = store_kind(summary.item.kind);
                    let format = Format::stored(&latest.content_type, kind);
                    let Some(text) = holtmail_pim::as_text(format, kind, &bytes) else {
                        totals.skipped += 1;
                        continue;
                    };
                    out.write_all(&text).await?;
                    totals.bytes += text.len() as u64;
                    if !text.ends_with(b"\n") {
                        out.write_all(b"\r\n").await?;
                        totals.bytes += 2;
                    }
                    totals.items += 1;
                    totals.versions += 1;
                    progress(&totals);
                }
            }
            PimFormat::Zip => {
                let mut zip = zipped::StreamZip::new();
                let mut manifest_items: Vec<Value> = Vec::new();
                for summary in &items {
                    let item = &summary.item;
                    let directory = self
                        .directories
                        .get(&item.collection_id)
                        .cloned()
                        .unwrap_or_else(|| "_".into());
                    let title = summary
                        .index
                        .as_ref()
                        .and_then(|i| i.title.as_deref())
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .unwrap_or("Untitled");
                    let folder = format!("{directory}/{} {}", names::segment(title, 60), item.id);
                    let mut versions: Vec<Value> = Vec::new();
                    for version in self.exporter.store.item_versions(item.id).await? {
                        let Some(bytes) = self.bytes(&version).await? else {
                            totals.skipped += 1;
                            continue;
                        };
                        let name = format!(
                            "{folder}/{} {}.{}",
                            version.number,
                            seen(version.seen_at),
                            extension(&version.content_type)
                        );
                        let size = bytes.len() as u64;
                        zip.add(out, name.clone(), version.seen_at, bytes).await?;
                        totals.versions += 1;
                        totals.bytes += size;
                        versions.push(json!({
                            "number": version.number,
                            "file": name,
                            "seen_at": version.seen_at.as_millis(),
                            "content_type": version.content_type,
                            "blob_id": version.blob_id.to_string(),
                            "etag": version.etag,
                        }));
                    }
                    manifest_items.push(json!({
                        "id": item.id.to_string(),
                        "collection_id": item.collection_id.to_string(),
                        "remote_id": item.remote_id,
                        "kind": store_kind(item.kind).as_str(),
                        "title": summary.index.as_ref().and_then(|i| i.title.clone()),
                        "uid": summary.index.as_ref().and_then(|i| i.uid.clone()),
                        "first_seen_at": item.first_seen_at.as_millis(),
                        "removed_at": item.removed_at.map(|t| t.as_millis()),
                        "directory": folder,
                        "versions": versions,
                    }));
                    totals.items += 1;
                    progress(&totals);
                }
                let manifest = json!({
                    "format": "holtmail-pim-export",
                    "version": 1,
                    "archive": { "id": self.archive.id.to_string(), "name": self.archive.name },
                    "kind": match self.selection.kind {
                        PimKind::Contacts => "contacts",
                        PimKind::Calendar => "calendar",
                    },
                    "exported_at": Timestamp::now().as_millis(),
                    "collections": self.collections.iter().map(|c| json!({
                        "id": c.id.to_string(),
                        "name": c.name,
                        "directory": self.directories.get(&c.id),
                        "account_id": c.account_id.map(|a| a.to_string()),
                        "removed_at": c.removed_at.map(|t| t.as_millis()),
                    })).collect::<Vec<_>>(),
                    "items": manifest_items,
                });
                let text = serde_json::to_vec_pretty(&manifest)
                    .map_err(|e| ExportError::Io(std::io::Error::other(e)))?;
                zip.add(
                    out,
                    "manifest.json".into(),
                    Timestamp::now(),
                    Bytes::from(text),
                )
                .await?;
                zip.finish(out).await?;
            }
        }
        out.flush().await?;
        Ok(totals)
    }
}
