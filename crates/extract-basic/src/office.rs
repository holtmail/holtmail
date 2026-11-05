use std::io::{Cursor, Read};

use holtmail_extract::{ExtractError, MAX_TEXT};
use quick_xml::Reader;
use quick_xml::events::{BytesRef, Event};

use crate::MAX_UNPACKED;

const MAX_ENTRIES: usize = 10_000;
const MAX_PARTS: usize = 1_000;
const MAX_SHARED_STRINGS: usize = 1_000_000;

struct Package {
    zip: zip::ZipArchive<Cursor<Vec<u8>>>,
    unpacked: usize,
}

impl Package {
    fn open(content: &[u8]) -> Result<Self, ExtractError> {
        let zip = zip::ZipArchive::new(Cursor::new(content.to_vec())).map_err(unreadable)?;
        if zip.len() > MAX_ENTRIES {
            return Err(ExtractError::TooLarge);
        }
        Ok(Self { zip, unpacked: 0 })
    }

    fn names(&self, prefix: &str, suffix: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .zip
            .file_names()
            .filter_map(Result::ok)
            .filter(|n| n.starts_with(prefix) && n.ends_with(suffix))
            .map(|n| n.into_owned())
            .collect();
        names.sort_by_key(|n| {
            let digits: String = n[prefix.len()..n.len() - suffix.len()]
                .chars()
                .filter(char::is_ascii_digit)
                .collect();
            (digits.parse::<u64>().unwrap_or(u64::MAX), n.clone())
        });
        names.truncate(MAX_PARTS);
        names
    }

    fn part(&mut self, name: &str) -> Result<Option<Vec<u8>>, ExtractError> {
        let file = match self.zip.by_name(name) {
            Ok(file) => file,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(error) => return Err(unreadable(error)),
        };
        let budget = MAX_UNPACKED - self.unpacked;
        let mut out = Vec::new();
        file.take(budget as u64 + 1)
            .read_to_end(&mut out)
            .map_err(unreadable)?;
        if out.len() > budget {
            return Err(ExtractError::TooLarge);
        }
        self.unpacked += out.len();
        Ok(Some(out))
    }

    fn required(&mut self, name: &str) -> Result<Vec<u8>, ExtractError> {
        self.part(name)?
            .ok_or_else(|| ExtractError::Unreadable(format!("no {name}")))
    }
}

struct Rules {
    text_in: Option<&'static [&'static [u8]]>,
    line_after: &'static [&'static [u8]],
    space_at: &'static [&'static [u8]],
}

const WORD: Rules = Rules {
    text_in: Some(&[b"t"]),
    line_after: &[b"p", b"tr"],
    space_at: &[b"tab", b"br", b"tc"],
};

const SLIDES: Rules = Rules {
    text_in: Some(&[b"t"]),
    line_after: &[b"p"],
    space_at: &[b"br"],
};

const OPEN_DOCUMENT: Rules = Rules {
    text_in: None,
    line_after: &[b"p", b"h", b"table-row"],
    space_at: &[b"s", b"tab", b"line-break", b"table-cell"],
};

pub(crate) fn docx(content: &[u8]) -> Result<String, ExtractError> {
    let mut package = Package::open(content)?;
    let mut out = String::new();
    let main = package.required("word/document.xml")?;
    xml_text(&main, &WORD, &mut out)?;
    for part in ["word/footnotes.xml", "word/endnotes.xml"] {
        if let Some(xml) = package.part(part)? {
            xml_text(&xml, &WORD, &mut out)?;
        }
    }
    Ok(out)
}

pub(crate) fn pptx(content: &[u8]) -> Result<String, ExtractError> {
    let mut package = Package::open(content)?;
    let mut out = String::new();
    for name in package.names("ppt/slides/slide", ".xml") {
        if out.len() > MAX_TEXT {
            break;
        }
        let xml = package.required(&name)?;
        xml_text(&xml, &SLIDES, &mut out)?;
    }
    Ok(out)
}

pub(crate) fn open_document(content: &[u8]) -> Result<String, ExtractError> {
    let mut package = Package::open(content)?;
    let mut out = String::new();
    let xml = package.required("content.xml")?;
    xml_text(&xml, &OPEN_DOCUMENT, &mut out)?;
    Ok(out)
}

pub(crate) fn xlsx(content: &[u8]) -> Result<String, ExtractError> {
    let mut package = Package::open(content)?;
    let shared = match package.part("xl/sharedStrings.xml")? {
        Some(xml) => shared_strings(&xml)?,
        None => Vec::new(),
    };
    let sheets = package.names("xl/worksheets/sheet", ".xml");
    if sheets.is_empty() {
        return Err(ExtractError::Unreadable("no worksheets".into()));
    }
    let mut out = String::new();
    for name in sheets {
        if out.len() > MAX_TEXT {
            break;
        }
        let xml = package.required(&name)?;
        sheet(&xml, &shared, &mut out)?;
    }
    Ok(out)
}

fn shared_strings(xml: &[u8]) -> Result<Vec<String>, ExtractError> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut strings = Vec::new();
    let mut current = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event_into(&mut buf).map_err(unreadable)? {
            Event::Start(e) if e.local_name().as_ref() == b"si" => current.clear(),
            Event::Start(e) if e.local_name().as_ref() == b"t" => in_text = true,
            Event::End(e) if e.local_name().as_ref() == b"t" => in_text = false,
            Event::End(e) if e.local_name().as_ref() == b"si" => {
                if strings.len() >= MAX_SHARED_STRINGS {
                    return Err(ExtractError::TooLarge);
                }
                strings.push(std::mem::take(&mut current));
            }
            Event::Empty(e) if e.local_name().as_ref() == b"si" => strings.push(String::new()),
            Event::Text(e) if in_text => current.push_str(&e.decode().map_err(unreadable)?),
            Event::CData(e) if in_text => current.push_str(&e.decode().map_err(unreadable)?),
            Event::GeneralRef(e) if in_text => push_ref(&e, &mut current),
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(strings)
}

fn sheet(xml: &[u8], shared: &[String], out: &mut String) -> Result<(), ExtractError> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut cell_type = Vec::new();
    let mut in_value = false;
    let mut value = String::new();
    loop {
        match reader.read_event_into(&mut buf).map_err(unreadable)? {
            Event::Start(e) if e.local_name().as_ref() == b"c" => {
                cell_type = e
                    .try_get_attribute("t")
                    .ok()
                    .flatten()
                    .map(|a| a.value.into_owned())
                    .unwrap_or_default();
            }
            Event::Start(e) if matches!(e.local_name().as_ref(), b"v" | b"t") => {
                in_value = true;
                value.clear();
            }
            Event::End(e) if matches!(e.local_name().as_ref(), b"v" | b"t") => {
                in_value = false;
                let text = if cell_type == b"s" {
                    value
                        .trim()
                        .parse::<usize>()
                        .ok()
                        .and_then(|i| shared.get(i))
                        .map(String::as_str)
                        .unwrap_or_default()
                } else {
                    value.as_str()
                };
                if !text.is_empty() {
                    out.push_str(text);
                    out.push(' ');
                }
            }
            Event::End(e) if e.local_name().as_ref() == b"row" => {
                out.push('\n');
                if out.len() > MAX_TEXT {
                    break;
                }
            }
            Event::Text(e) if in_value => value.push_str(&e.decode().map_err(unreadable)?),
            Event::GeneralRef(e) if in_value => push_ref(&e, &mut value),
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

fn xml_text(xml: &[u8], rules: &Rules, out: &mut String) -> Result<(), ExtractError> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let collecting = |depth: usize| rules.text_in.is_none() || depth > 0;
    let watched = |name: &[u8]| rules.text_in.is_some_and(|names| names.contains(&name));
    loop {
        match reader.read_event_into(&mut buf).map_err(unreadable)? {
            Event::Start(e) => {
                let name = e.local_name();
                if watched(name.as_ref()) {
                    depth += 1;
                }
                if rules.space_at.contains(&name.as_ref()) {
                    out.push(' ');
                }
            }
            Event::Empty(e) => {
                let name = e.local_name();
                if rules.space_at.contains(&name.as_ref()) {
                    out.push(' ');
                }
                if rules.line_after.contains(&name.as_ref()) {
                    out.push('\n');
                }
            }
            Event::End(e) => {
                let name = e.local_name();
                if watched(name.as_ref()) {
                    depth = depth.saturating_sub(1);
                }
                if rules.line_after.contains(&name.as_ref()) {
                    out.push('\n');
                    if out.len() > MAX_TEXT {
                        break;
                    }
                }
            }
            Event::Text(e) if collecting(depth) => out.push_str(&e.decode().map_err(unreadable)?),
            Event::CData(e) if collecting(depth) => out.push_str(&e.decode().map_err(unreadable)?),
            Event::GeneralRef(e) if collecting(depth) => push_ref(&e, out),
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

fn push_ref(reference: &BytesRef<'_>, out: &mut String) {
    if let Ok(Some(c)) = reference.resolve_char_ref() {
        out.push(c);
        return;
    }
    let name: &[u8] = reference.as_ref();
    out.push(match name {
        b"amp" => '&',
        b"lt" => '<',
        b"gt" => '>',
        b"quot" => '"',
        b"apos" => '\'',
        _ => return,
    });
}

fn unreadable(error: impl std::fmt::Display) -> ExtractError {
    ExtractError::Unreadable(error.to_string())
}
