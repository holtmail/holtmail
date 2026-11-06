use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use holtmail_core::{ArchiveId, CollectionId, FolderId, Timestamp};
use holtmail_export::{
    Exporter, Format, PimFormat, PimKind, PimSelection, PimTotals, Selection, Totals,
};
use holtmail_index_tantivy::TantivyIndex;
use holtmail_store::CollectionKind;
use holtmail_store::Store;

use crate::config::{self, Config};

pub struct ExportArgs {
    pub archive: String,
    pub folder: Option<String>,
    pub label: Option<String>,
    pub query: Option<String>,
    pub format: Option<String>,
    pub output: PathBuf,
}

pub async fn find_archive(store: &dyn Store, wanted: &str) -> anyhow::Result<ArchiveId> {
    let archives = store.list_archives().await?;
    if let Some(archive) = archives.iter().find(|a| a.id.to_string() == wanted) {
        return Ok(archive.id);
    }
    let named: Vec<_> = archives.iter().filter(|a| a.name == wanted).collect();
    match named.as_slice() {
        [one] => Ok(one.id),
        [] => bail!("no archive named {wanted:?}; `holtmail archives` lists them"),
        _ => bail!("several archives are named {wanted:?}; pass the id instead"),
    }
}

/// The folder by id, by path, or by `account name/path` when several accounts have the path.
pub async fn find_folder(
    store: &dyn Store,
    archive: ArchiveId,
    wanted: &str,
) -> anyhow::Result<FolderId> {
    let folders = store.list_folders(archive).await?;
    let accounts = store.list_accounts(archive).await?;
    if let Some(found) = folders.iter().find(|f| f.folder.id.to_string() == wanted) {
        return Ok(found.folder.id);
    }
    let matching: Vec<_> = folders
        .iter()
        .filter(|f| {
            f.folder.path == wanted
                || accounts.iter().any(|a| {
                    a.id == f.folder.account_id && format!("{}/{}", a.name, f.folder.path) == wanted
                })
        })
        .collect();
    match matching.as_slice() {
        [one] => Ok(one.folder.id),
        [] => bail!("no folder {wanted:?} in this archive"),
        several => {
            let ids: Vec<String> = several.iter().map(|f| f.folder.id.to_string()).collect();
            bail!(
                "several accounts have a folder {wanted:?}; pass `account name/{wanted}` or one of the ids {}",
                ids.join(", ")
            )
        }
    }
}

fn format_for(raw: Option<&str>, output: &Path) -> anyhow::Result<Format> {
    match raw {
        Some(raw) => Format::parse(raw)
            .with_context(|| format!("unknown format {raw:?}; use mbox, zip or pdf")),
        None => Ok(
            match output
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
            {
                Some(ext) if ext == "zip" => Format::Zip,
                _ => Format::Mbox,
            },
        ),
    }
}

pub async fn run(config: &Config, args: ExportArgs) -> anyhow::Result<()> {
    let format = format_for(args.format.as_deref(), &args.output)?;
    let store = config::open_store(config).await?;
    let blobs = config::open_blobs(config).await?;
    let archive = find_archive(&*store, &args.archive).await?;
    let selection = match (&args.folder, &args.label, &args.query) {
        (None, None, None) => Selection::All,
        (Some(folder), None, None) => {
            Selection::Folder(find_folder(&*store, archive, folder).await?)
        }
        (None, Some(label), None) => Selection::Label(label.clone()),
        (None, None, Some(query)) => Selection::Search(
            holtmail_query::parse(query).map_err(|e| anyhow::anyhow!("--query: {e}"))?,
        ),
        _ => bail!("give at most one of --folder, --label and --query"),
    };
    let mut exporter = Exporter::new(blobs, store.clone());
    if matches!(selection, Selection::Search(_)) {
        let index = TantivyIndex::open(config.data_dir.join("index")).context(
            "opening the search index; while `holtmail serve` runs, export a search from the web app instead",
        )?;
        exporter = exporter.with_index(Arc::new(index));
    }
    let export = exporter.prepare(archive, selection, format).await?;
    let progress = |t: &Totals| eprintln!("  {} messages", t.messages);
    let totals = if args.output == Path::new("-") {
        let mut out = tokio::io::stdout();
        export.write(&mut out, &progress).await?
    } else {
        write_file(&args.output, |mut file| async move {
            let totals = export.write(&mut file, &progress).await?;
            file.sync_all().await?;
            Ok(totals)
        })
        .await?
    };
    eprintln!(
        "Exported {} messages ({} MB){}.",
        totals.messages,
        totals.bytes / 1_000_000,
        if totals.skipped > 0 {
            format!(
                "; {} left out because their bytes are missing or damaged",
                totals.skipped
            )
        } else {
            String::new()
        }
    );
    Ok(())
}

pub struct PimArgs {
    pub archive: String,
    pub calendar: bool,
    pub collection: Option<String>,
    pub person: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub include_removed: bool,
    pub format: Option<String>,
    pub output: PathBuf,
}

fn pim_format_for(raw: Option<&str>, output: &Path, kind: PimKind) -> anyhow::Result<PimFormat> {
    match raw {
        Some(raw) => PimFormat::parse(raw, kind).with_context(|| {
            format!(
                "unknown format {raw:?}; use {} or zip",
                PimFormat::Text.extension(kind)
            )
        }),
        None => Ok(
            match output
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
            {
                Some(ext) if ext == "zip" => PimFormat::Zip,
                _ => PimFormat::Text,
            },
        ),
    }
}

/// Midnight UTC of `YYYY-MM-DD`.
fn day(raw: &str) -> anyhow::Result<Timestamp> {
    let date = chrono::NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .with_context(|| format!("{raw:?} is not a day like 2025-03-01"))?;
    Ok(Timestamp::from_millis(
        date.and_hms_opt(0, 0, 0)
            .context("midnight")?
            .and_utc()
            .timestamp_millis(),
    ))
}

/// The address book or calendar by id or by name.
async fn find_collection(
    store: &dyn Store,
    archive: ArchiveId,
    kind: CollectionKind,
    wanted: &str,
) -> anyhow::Result<CollectionId> {
    let collections: Vec<_> = store
        .list_collections(archive)
        .await?
        .into_iter()
        .map(|c| c.collection)
        .filter(|c| c.kind == kind)
        .collect();
    if let Some(found) = collections.iter().find(|c| c.id.to_string() == wanted) {
        return Ok(found.id);
    }
    let named: Vec<_> = collections.iter().filter(|c| c.name == wanted).collect();
    match named.as_slice() {
        [one] => Ok(one.id),
        [] => bail!("no {} named {wanted:?} in this archive", kind_name(kind)),
        several => {
            let ids: Vec<String> = several.iter().map(|c| c.id.to_string()).collect();
            bail!(
                "several {}s are named {wanted:?}; pass one of the ids {}",
                kind_name(kind),
                ids.join(", ")
            )
        }
    }
}

fn kind_name(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Contacts => "address book",
        CollectionKind::Calendar => "calendar",
    }
}

pub async fn run_pim(config: &Config, args: PimArgs) -> anyhow::Result<()> {
    let kind = if args.calendar {
        PimKind::Calendar
    } else {
        PimKind::Contacts
    };
    let format = pim_format_for(args.format.as_deref(), &args.output, kind)?;
    let store = config::open_store(config).await?;
    let blobs = config::open_blobs(config).await?;
    let archive = find_archive(&*store, &args.archive).await?;
    let collection = match &args.collection {
        Some(wanted) => Some(
            find_collection(
                &*store,
                archive,
                match kind {
                    PimKind::Contacts => CollectionKind::Contacts,
                    PimKind::Calendar => CollectionKind::Calendar,
                },
                wanted,
            )
            .await?,
        ),
        None => None,
    };
    let range = match (&args.from, &args.to) {
        (Some(from), Some(to)) => Some((day(from)?, day(to)?)),
        _ => None,
    };
    let selection = PimSelection {
        kind,
        collection,
        person: args
            .person
            .as_deref()
            .map(|p| {
                p.parse()
                    .map_err(|_| anyhow::anyhow!("--person takes a person id"))
            })
            .transpose()?,
        range,
        removed: args.include_removed,
    };
    let export = Exporter::new(blobs, store.clone())
        .prepare_pim(archive, selection, format)
        .await?;
    let progress = |_: &PimTotals| {};
    let totals = if args.output == Path::new("-") {
        let mut out = tokio::io::stdout();
        export.write(&mut out, &progress).await?
    } else {
        write_file(&args.output, |mut file| async move {
            let totals = export.write(&mut file, &progress).await?;
            file.sync_all().await?;
            Ok(totals)
        })
        .await?
    };
    eprintln!(
        "Exported {} {} ({} versions, {} KB){}.",
        totals.items,
        match kind {
            PimKind::Contacts => "contacts",
            PimKind::Calendar => "calendar items",
        },
        totals.versions,
        totals.bytes / 1000,
        if totals.skipped > 0 {
            format!(
                "; {} versions left out because their bytes are missing, damaged or unreadable",
                totals.skipped
            )
        } else {
            String::new()
        }
    );
    Ok(())
}

/// Writes to `<path>.part` and renames it to `path` when `write` succeeds, so a failed write never
/// leaves a file that looks complete.
pub async fn write_file<T, F, Fut>(path: &Path, write: F) -> anyhow::Result<T>
where
    F: FnOnce(tokio::fs::File) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    anyhow::ensure!(
        !path.exists(),
        "{} exists; not overwriting it",
        path.display()
    );
    let mut partial = path.as_os_str().to_owned();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .await
        .with_context(|| format!("creating {}", partial.display()))?;
    match write(file).await {
        Ok(value) => {
            tokio::fs::rename(&partial, path).await?;
            Ok(value)
        }
        Err(error) => {
            let _ = tokio::fs::remove_file(&partial).await;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_format() {
        assert_eq!(format_for(None, Path::new("a.zip")).unwrap(), Format::Zip);
        assert_eq!(format_for(None, Path::new("a.ZIP")).unwrap(), Format::Zip);
        assert_eq!(format_for(None, Path::new("-")).unwrap(), Format::Mbox);
        assert_eq!(
            format_for(Some("zip"), Path::new("a.mbox")).unwrap(),
            Format::Zip
        );
        assert!(format_for(Some("rar"), Path::new("a")).is_err());
        assert_eq!(
            format_for(Some("pdf"), Path::new("a.zip")).unwrap(),
            holtmail_export::Format::Pdf
        );
    }

    #[test]
    fn picks_the_contacts_and_calendar_format() {
        let pick = |raw, path, kind| pim_format_for(raw, Path::new(path), kind).ok();
        assert_eq!(
            pick(None, "a.vcf", PimKind::Contacts),
            Some(PimFormat::Text)
        );
        assert_eq!(pick(None, "a.ZIP", PimKind::Calendar), Some(PimFormat::Zip));
        assert_eq!(
            pick(Some("ics"), "a.zip", PimKind::Calendar),
            Some(PimFormat::Text)
        );
        assert_eq!(pick(Some("ics"), "a", PimKind::Contacts), None);
        assert_eq!(
            day("2025-03-01").unwrap(),
            Timestamp::from_millis(1_740_787_200_000)
        );
        assert!(day("March").is_err());
    }
}
