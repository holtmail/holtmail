//! Reading messages out of mailbox files: mbox, Maildir, `.eml` files and zip archives of them;
//! and address books and calendars out of `.vcf` and `.ics` files, alone, in directories or in
//! zips such as Google Takeout.
//!
//! [`open`] detects the format and returns an iterator of [`ImportedMessage`]s, each with the
//! folder it belongs to. Files are read one message at a time, so large archives do not need to fit
//! in memory. [`collections`] returns the `.vcf` and `.ics` files of the same path, whole.

use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use bytes::Bytes;
use holtmail_core::Timestamp;
use mail_parser::mailbox::{maildir, mbox};

/// One message read from a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedMessage {
    /// Folder path with `/` between levels, from the file layout; `None` for the top level.
    pub folder: Option<String>,
    /// The message bytes as stored in the file.
    pub raw: Bytes,
    /// IMAP-style flags known from the file format, e.g. `\Seen` from Maildir.
    pub flags: Vec<String>,
    /// When the message was received, when the format records it.
    pub received_at: Option<Timestamp>,
}

/// A format [`open`] can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// One mbox file.
    Mbox,
    /// A Maildir or Maildir++ directory.
    Maildir,
    /// One `.eml` file.
    Eml,
    /// A zip archive of `.eml` and `.mbox` files.
    Zip,
    /// A directory tree of `.eml` and `.mbox` files; subdirectories are folders.
    Tree,
    /// One vCard file (`.vcf`, `.vcard`): an address book, no messages.
    VCard,
    /// One iCalendar file (`.ics`, `.ical`, `.ifb`): a calendar, no messages.
    ICalendar,
}

/// Why reading failed.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// Reading a file failed.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        #[source]
        source: std::io::Error,
    },
    /// The zip archive is damaged.
    #[error("{path}: {source}")]
    Zip {
        /// The archive.
        path: PathBuf,
        /// What went wrong.
        #[source]
        source: zip::result::ZipError,
    },
    /// The path is not a format Holtmail reads.
    #[error("{0}: not an mbox, Maildir, .eml, .vcf, .ics or zip")]
    UnknownFormat(PathBuf),
    /// An address book or calendar file is larger than [`MAX_COLLECTION_FILE`].
    #[error("{0}: larger than {MAX_COLLECTION_FILE} bytes")]
    TooLarge(PathBuf),
}

/// The largest `.vcf` or `.ics` file [`collections`] reads; each is read whole.
pub const MAX_COLLECTION_FILE: u64 = 256 << 20;

/// What a [`CollectionFile`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionFormat {
    /// vCard: an address book.
    VCard,
    /// iCalendar: a calendar.
    ICalendar,
}

/// One address book or calendar file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionFile {
    /// The file's path inside what was opened, with `/` between levels, such as
    /// `Takeout/Contacts/My Contacts/My Contacts.vcf`; the file name for a single file.
    pub path: String,
    /// The file name without its extension.
    pub name: String,
    /// vCard or iCalendar.
    pub format: CollectionFormat,
    /// The whole file.
    pub bytes: Vec<u8>,
}

/// Messages read from one source, in file order.
pub type Messages = Box<dyn Iterator<Item = Result<ImportedMessage, ImportError>> + Send>;

/// The format of `path`, judged by its extension and layout.
pub fn detect(path: &Path) -> Option<Format> {
    if path.is_dir() {
        return Some(if is_maildir(path) {
            Format::Maildir
        } else {
            Format::Tree
        });
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "mbox" | "mbx" => Some(Format::Mbox),
        "eml" => Some(Format::Eml),
        "zip" => Some(Format::Zip),
        _ => match collection_format(path) {
            Some(CollectionFormat::VCard) => Some(Format::VCard),
            Some(CollectionFormat::ICalendar) => Some(Format::ICalendar),
            None => sniff_mbox(path).then_some(Format::Mbox),
        },
    }
}

fn collection_format(path: &Path) -> Option<CollectionFormat> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "vcf" | "vcard" => Some(CollectionFormat::VCard),
        "ics" | "ical" | "ifb" => Some(CollectionFormat::ICalendar),
        _ => None,
    }
}

/// Every `.vcf` and `.ics` file in `path`: the file itself, the files of a directory tree, or the
/// entries of a zip (Google Takeout puts them under `Contacts/<group>/` and `Calendar/`), sorted
/// by path. Other files are skipped; an mbox or `.eml` gives none. Each file is read whole.
pub fn collections(path: &Path) -> Result<Vec<CollectionFile>, ImportError> {
    let format = detect(path).ok_or_else(|| ImportError::UnknownFormat(path.to_owned()))?;
    let mut out = Vec::new();
    match format {
        Format::VCard | Format::ICalendar => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push(read_collection(path, &name)?);
        }
        Format::Tree => {
            let mut files = Vec::new();
            collect_collections(path, &mut files)?;
            for file in files {
                let relative = file.strip_prefix(path).unwrap_or(&file);
                out.push(read_collection(&file, &slash_path(relative))?);
            }
        }
        Format::Zip => {
            let file = File::open(path).map_err(io(path))?;
            let zip_error = |source| ImportError::Zip {
                path: path.to_owned(),
                source,
            };
            let mut archive = zip::ZipArchive::new(file).map_err(zip_error)?;
            for index in 0..archive.len() {
                let entry = archive.by_index(index).map_err(zip_error)?;
                let Some(name) = entry.enclosed_name() else {
                    continue;
                };
                let Some(format) = collection_format(&name).filter(|_| !entry.is_dir()) else {
                    continue;
                };
                if entry.size() > MAX_COLLECTION_FILE {
                    return Err(ImportError::TooLarge(path.join(&name)));
                }
                let mut bytes = Vec::new();
                entry
                    .take(MAX_COLLECTION_FILE + 1)
                    .read_to_end(&mut bytes)
                    .map_err(io(&path.join(&name)))?;
                if bytes.len() as u64 > MAX_COLLECTION_FILE {
                    return Err(ImportError::TooLarge(path.join(&name)));
                }
                out.push(CollectionFile {
                    path: slash_path(&name),
                    name: stem(&name),
                    format,
                    bytes,
                });
            }
        }
        Format::Mbox | Format::Maildir | Format::Eml => {}
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().trim().to_owned())
        .unwrap_or_default()
}

fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

fn read_collection(path: &Path, relative: &str) -> Result<CollectionFile, ImportError> {
    let format =
        collection_format(path).ok_or_else(|| ImportError::UnknownFormat(path.to_owned()))?;
    let size = fs::metadata(path).map_err(io(path))?.len();
    if size > MAX_COLLECTION_FILE {
        return Err(ImportError::TooLarge(path.to_owned()));
    }
    Ok(CollectionFile {
        path: relative.to_owned(),
        name: stem(path),
        format,
        bytes: fs::read(path).map_err(io(path))?,
    })
}

fn collect_collections(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), ImportError> {
    for entry in fs::read_dir(dir).map_err(io(dir))? {
        let path = entry.map_err(io(dir))?.path();
        if path.is_dir() {
            collect_collections(&path, files)?;
        } else if collection_format(&path).is_some() {
            files.push(path);
        }
    }
    Ok(())
}

/// Reads every message in `path`, detecting its format.
pub fn open(path: &Path) -> Result<Messages, ImportError> {
    let format = detect(path).ok_or_else(|| ImportError::UnknownFormat(path.to_owned()))?;
    open_as(path, format)
}

/// Reads every message in `path` as `format`.
pub fn open_as(path: &Path, format: Format) -> Result<Messages, ImportError> {
    match format {
        Format::Mbox => read_mbox(path, folder_from_file(path)),
        Format::Maildir => read_maildir(path),
        Format::Eml => {
            let raw = fs::read(path).map_err(io(path))?;
            Ok(Box::new(std::iter::once(Ok(eml(raw, None)))))
        }
        Format::Zip => read_zip(path),
        Format::Tree => read_tree(path),
        Format::VCard | Format::ICalendar => Ok(Box::new(std::iter::empty())),
    }
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> ImportError + '_ {
    move |source| ImportError::Io {
        path: path.to_owned(),
        source,
    }
}

fn eml(raw: Vec<u8>, folder: Option<String>) -> ImportedMessage {
    ImportedMessage {
        folder,
        raw: Bytes::from(strip_preamble(raw)),
        flags: Vec::new(),
        received_at: None,
    }
}

/// Some exporters put a line before the headers, such as Outlook's `Microsoft Mail Internet
/// Headers Version 2.0` or an mbox `From ` line in a single `.eml`.
fn strip_preamble(raw: Vec<u8>) -> Vec<u8> {
    let first_line_end = raw.iter().position(|&b| b == b'\n').unwrap_or(raw.len());
    let first = &raw[..first_line_end];
    let is_header = first
        .iter()
        .position(|&b| b == b':')
        .is_some_and(|colon| colon > 0 && !first[..colon].contains(&b' '));
    if !is_header
        && (first.starts_with(b"From ") || first.starts_with(b"Microsoft Mail Internet Headers"))
    {
        return raw[(first_line_end + 1).min(raw.len())..].to_vec();
    }
    raw
}

fn folder_from_file(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?.trim();
    (!stem.is_empty()).then(|| stem.to_owned())
}

fn sniff_mbox(path: &Path) -> bool {
    let mut head = [0u8; 5];
    File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && &head == b"From "
}

fn read_mbox(path: &Path, folder: Option<String>) -> Result<Messages, ImportError> {
    let file = File::open(path).map_err(io(path))?;
    let owned = path.to_owned();
    let messages = mbox::MessageIterator::new(BufReader::new(file)).map(move |message| {
        let message = message.map_err(|source| ImportError::Io {
            path: owned.clone(),
            source,
        })?;
        let date = message.internal_date();
        Ok(ImportedMessage {
            folder: folder.clone(),
            received_at: (date > 0).then(|| Timestamp::from_secs(i64::try_from(date).unwrap_or(0))),
            flags: Vec::new(),
            raw: Bytes::from(without_separator_line(message.unwrap_contents())),
        })
    });
    Ok(Box::new(messages))
}

/// Drops the empty line an mbox puts after each message: it belongs to the file, not the message.
fn without_separator_line(mut raw: Vec<u8>) -> Vec<u8> {
    if raw.ends_with(b"\n\n") {
        raw.pop();
    } else if raw.ends_with(b"\n\r\n") {
        raw.truncate(raw.len() - 2);
    }
    raw
}

fn is_maildir(path: &Path) -> bool {
    path.join("cur").is_dir()
        || fs::read_dir(path).is_ok_and(|entries| {
            entries.flatten().any(|e| {
                e.file_name().to_string_lossy().starts_with('.') && e.path().join("cur").is_dir()
            })
        })
}

fn read_maildir(path: &Path) -> Result<Messages, ImportError> {
    let plus_plus = fs::read_dir(path)
        .map_err(io(path))?
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with('.') && e.path().join("cur").is_dir());
    let prefix = if plus_plus { Some(".") } else { None };
    let folders = maildir::FolderIterator::new(path, prefix).map_err(io(path))?;
    let owned = path.to_owned();
    let error = move |source| ImportError::Io {
        path: owned.clone(),
        source,
    };
    let messages = folders.flat_map(move |folder| -> Messages {
        let error = error.clone();
        match folder {
            Err(source) => Box::new(std::iter::once(Err(error(source)))),
            Ok(folder) => {
                let name = folder.name().map(|n| n.replace('.', "/"));
                Box::new(folder.map(move |message| {
                    let message = message.map_err(&error)?;
                    let date = message.internal_date();
                    Ok(ImportedMessage {
                        folder: name.clone(),
                        received_at: (date > 0)
                            .then(|| Timestamp::from_secs(i64::try_from(date).unwrap_or(0))),
                        flags: message.flags().iter().map(maildir_flag).collect(),
                        raw: Bytes::from(message.unwrap_contents()),
                    })
                }))
            }
        }
    });
    Ok(Box::new(messages))
}

fn maildir_flag(flag: &maildir::Flag) -> String {
    match flag {
        maildir::Flag::Seen => "\\Seen",
        maildir::Flag::Replied => "\\Answered",
        maildir::Flag::Flagged => "\\Flagged",
        maildir::Flag::Draft => "\\Draft",
        maildir::Flag::Trashed => "\\Deleted",
        maildir::Flag::Passed => "$Forwarded",
    }
    .to_owned()
}

fn read_zip(path: &Path) -> Result<Messages, ImportError> {
    let file = File::open(path).map_err(io(path))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|source| ImportError::Zip {
        path: path.to_owned(),
        source,
    })?;
    let owned = path.to_owned();
    let mut index = 0;
    let messages = std::iter::from_fn(move || {
        while index < archive.len() {
            let current = index;
            index += 1;
            let mut entry = match archive.by_index(current) {
                Ok(entry) => entry,
                Err(source) => {
                    return Some(Err(ImportError::Zip {
                        path: owned.clone(),
                        source,
                    }));
                }
            };
            let Some(name) = entry.enclosed_name() else {
                continue;
            };
            if entry.is_dir() || !has_extension(&name, "eml") {
                continue;
            }
            let mut raw = Vec::new();
            if let Err(source) = entry.read_to_end(&mut raw) {
                return Some(Err(ImportError::Io {
                    path: owned.join(&name),
                    source,
                }));
            }
            return Some(Ok(eml(raw, folder_of(&name))));
        }
        None
    });
    Ok(Box::new(messages))
}

fn read_tree(root: &Path) -> Result<Messages, ImportError> {
    let mut files = Vec::new();
    collect(root, &mut files)?;
    files.sort();
    let root = root.to_owned();
    let messages = files.into_iter().flat_map(move |file| -> Messages {
        let relative = file.strip_prefix(&root).unwrap_or(&file).to_owned();
        if has_extension(&file, "eml") {
            Box::new(std::iter::once(
                fs::read(&file)
                    .map(|raw| eml(raw, folder_of(&relative)))
                    .map_err(|source| ImportError::Io {
                        path: file.clone(),
                        source,
                    }),
            ))
        } else {
            let folder = match (folder_of(&relative), folder_from_file(&file)) {
                (Some(parent), Some(stem)) => Some(format!("{parent}/{stem}")),
                (parent, stem) => stem.or(parent),
            };
            read_mbox(&file, folder).unwrap_or_else(|e| Box::new(std::iter::once(Err(e))))
        }
    });
    Ok(Box::new(messages))
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), ImportError> {
    for entry in fs::read_dir(dir).map_err(io(dir))? {
        let path = entry.map_err(io(dir))?.path();
        if path.is_dir() {
            collect(&path, files)?;
        } else if has_extension(&path, "eml") || has_extension(&path, "mbox") {
            files.push(path);
        }
    }
    Ok(())
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

fn folder_of(relative: &Path) -> Option<String> {
    let parent = relative.parent()?;
    let parts: Vec<_> = parent
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .filter(|c| !c.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    const ONE: &[u8] = b"From: ada@example.com\nSubject: One\n\nFirst\n";
    const TWO: &[u8] = b"From: bob@example.org\nSubject: Two\n\nSecond\n";

    fn all(path: &Path) -> Vec<ImportedMessage> {
        open(path).unwrap().map(Result::unwrap).collect()
    }

    fn subjects(messages: &[ImportedMessage]) -> Vec<(Option<String>, String)> {
        messages
            .iter()
            .map(|m| {
                let text = String::from_utf8_lossy(&m.raw);
                let subject = text
                    .lines()
                    .find_map(|l| l.strip_prefix("Subject: "))
                    .unwrap_or_default()
                    .to_owned();
                (m.folder.clone(), subject)
            })
            .collect()
    }

    #[test]
    fn reads_an_mbox_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Sent.mbox");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"From ada@example.com Fri Mar 14 09:00:00 2025\n")
            .unwrap();
        file.write_all(ONE).unwrap();
        file.write_all(b"\nFrom bob@example.org Fri Mar 14 10:00:00 2025\n")
            .unwrap();
        file.write_all(TWO).unwrap();
        drop(file);

        assert_eq!(detect(&path), Some(Format::Mbox));
        let messages = all(&path);
        assert_eq!(
            subjects(&messages),
            [
                (Some("Sent".into()), "One".into()),
                (Some("Sent".into()), "Two".into())
            ]
        );
        assert_eq!(
            messages[0].received_at,
            Some(Timestamp::from_secs(1_741_942_800))
        );
    }

    #[test]
    fn reads_mboxrd_back_to_the_original_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Quoted.mbox");
        fs::write(
            &path,
            b"From ada@example.com Fri Mar 14 09:00:00 2025\n\
              Subject: One\n\n>From here\n>>From there\n\n\n\
              From bob@example.org Fri Mar 14 10:00:00 2025\n\
              Subject: Two\r\n\r\nHi\r\n\r\n\
              From bob@example.org Fri Mar 14 11:00:00 2025\n\
              Subject: Three\n\nlast",
        )
        .unwrap();
        let raw: Vec<Bytes> = all(&path).into_iter().map(|m| m.raw).collect();
        assert_eq!(
            raw,
            [
                Bytes::from_static(b"Subject: One\n\nFrom here\n>From there\n\n"),
                Bytes::from_static(b"Subject: Two\r\n\r\nHi\r\n"),
                Bytes::from_static(b"Subject: Three\n\nlast"),
            ]
        );
    }

    #[test]
    fn reads_a_maildir_with_folders_and_flags() {
        let dir = tempfile::tempdir().unwrap();
        for folder in ["", ".Work", ".Work.Reports"] {
            for sub in ["cur", "new", "tmp"] {
                fs::create_dir_all(dir.path().join(folder).join(sub)).unwrap();
            }
        }
        fs::write(dir.path().join("cur/1700000000.1.host:2,SF"), ONE).unwrap();
        fs::write(dir.path().join(".Work.Reports/new/1700000001.2.host"), TWO).unwrap();

        assert_eq!(detect(dir.path()), Some(Format::Maildir));
        let mut messages = all(dir.path());
        messages.sort_by_key(|m| m.folder.clone());
        assert_eq!(
            subjects(&messages),
            [
                (None, "One".into()),
                (Some("Work/Reports".into()), "Two".into())
            ]
        );
        assert_eq!(messages[0].flags, ["\\Seen", "\\Flagged"]);
    }

    #[test]
    fn reads_a_tree_of_eml_and_mbox_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Projects/2025")).unwrap();
        fs::write(dir.path().join("top.eml"), ONE).unwrap();
        let mut outlook = b"Microsoft Mail Internet Headers Version 2.0\r\n".to_vec();
        outlook.extend_from_slice(TWO);
        fs::write(dir.path().join("Projects/2025/two.eml"), outlook).unwrap();
        fs::write(dir.path().join("notes.txt"), b"ignored").unwrap();

        assert_eq!(detect(dir.path()), Some(Format::Tree));
        let messages = all(dir.path());
        assert_eq!(
            subjects(&messages),
            [
                (Some("Projects/2025".into()), "Two".into()),
                (None, "One".into())
            ]
        );
        assert!(messages[0].raw.starts_with(b"From: bob"), "preamble kept");
    }

    #[test]
    fn reads_a_zip_of_eml_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("Inbox/one.eml", options).unwrap();
        zip.write_all(ONE).unwrap();
        zip.start_file("../escape.eml", options).unwrap();
        zip.write_all(TWO).unwrap();
        zip.start_file("readme.txt", options).unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();

        assert_eq!(
            subjects(&all(&path)),
            [(Some("Inbox".into()), "One".into())]
        );
    }

    #[test]
    fn rejects_unknown_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        fs::write(&path, b"hello").unwrap();
        assert!(matches!(open(&path), Err(ImportError::UnknownFormat(_))));
    }

    const CARD: &[u8] = b"BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada Quill\r\nEND:VCARD\r\n";
    const CALENDAR: &[u8] =
        b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:e@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn reads_a_single_address_book_without_messages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Friends.VCF");
        fs::write(&path, CARD).unwrap();
        assert_eq!(detect(&path), Some(Format::VCard));
        assert_eq!(all(&path).len(), 0);
        let files = collections(&path).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "Friends.VCF");
        assert_eq!(files[0].name, "Friends");
        assert_eq!(files[0].format, CollectionFormat::VCard);
        assert_eq!(files[0].bytes, CARD);
    }

    #[test]
    fn reads_a_takeout_zip_with_mail_contacts_and_calendars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("takeout-20250101T000000Z-001.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("Takeout/Contacts/My Contacts/My Contacts.vcf", options)
            .unwrap();
        zip.write_all(CARD).unwrap();
        zip.start_file("Takeout/Calendar/ada@example.com.ics", options)
            .unwrap();
        zip.write_all(CALENDAR).unwrap();
        zip.start_file("Takeout/Mail/Inbox/one.eml", options)
            .unwrap();
        zip.write_all(ONE).unwrap();
        zip.start_file("../escape.vcf", options).unwrap();
        zip.write_all(CARD).unwrap();
        zip.finish().unwrap();

        let files = collections(&path).unwrap();
        let found: Vec<_> = files
            .iter()
            .map(|f| (f.path.as_str(), f.name.as_str(), f.format))
            .collect();
        assert_eq!(
            found,
            [
                (
                    "Takeout/Calendar/ada@example.com.ics",
                    "ada@example.com",
                    CollectionFormat::ICalendar
                ),
                (
                    "Takeout/Contacts/My Contacts/My Contacts.vcf",
                    "My Contacts",
                    CollectionFormat::VCard
                ),
            ]
        );
        assert_eq!(subjects(&all(&path)).len(), 1, "mail is still read");
    }

    #[test]
    fn reads_collections_from_a_directory_tree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Calendar")).unwrap();
        fs::write(dir.path().join("Calendar/Team.ics"), CALENDAR).unwrap();
        fs::write(dir.path().join("people.vcf"), CARD).unwrap();
        let files = collections(dir.path()).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["Calendar/Team.ics", "people.vcf"]);
        assert!(all(dir.path()).is_empty());
    }

    #[test]
    fn an_mbox_has_no_collections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Sent.mbox");
        fs::write(
            &path,
            b"From a@example.com Fri Mar 14 09:00:00 2025\nSubject: x\n\nx\n",
        )
        .unwrap();
        assert!(collections(&path).unwrap().is_empty());
    }
}
