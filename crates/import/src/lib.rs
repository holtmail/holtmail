//! Reading messages out of mailbox files: mbox, Maildir, `.eml` files and zip archives of them.
//!
//! [`open`] detects the format and returns an iterator of [`ImportedMessage`]s, each with the
//! folder it belongs to. Files are read one message at a time, so large archives do not need to fit
//! in memory.

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
    #[error("{0}: not an mbox, Maildir, .eml or zip")]
    UnknownFormat(PathBuf),
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
        _ => sniff_mbox(path).then_some(Format::Mbox),
    }
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
            raw: Bytes::from(message.unwrap_contents()),
        })
    });
    Ok(Box::new(messages))
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
}
