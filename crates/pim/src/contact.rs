use calcard::common::{IanaString, IanaType, PartialDateTime};
use calcard::vcard::{
    VCard, VCardEntry, VCardParameterName, VCardParameterValue, VCardProperty, VCardValue,
};
use holtmail_blob::BlobId;
use serde::{Deserialize, Serialize};

use crate::keys::{normalize_email, normalize_phone};

/// A contact as read from one vCard (2.1, 3.0 or 4.0).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    /// `UID`.
    pub uid: Option<String>,
    /// `REV`, as written or as an ISO 8601 time.
    pub rev: Option<String>,
    /// `FN`.
    pub full_name: Option<String>,
    /// `N`, by component.
    pub name: Option<NameParts>,
    /// `NICKNAME`s.
    pub nicknames: Vec<String>,
    /// `EMAIL`s in file order.
    pub emails: Vec<Email>,
    /// `TEL`s in file order.
    pub phones: Vec<Phone>,
    /// `ORG` with its units joined by `, `.
    pub organization: Option<String>,
    /// `TITLE`.
    pub title: Option<String>,
    /// `ADR`s in file order.
    pub addresses: Vec<PostalAddress>,
    /// `BDAY` as `YYYY-MM-DD`, `--MM-DD` when the year is unknown, or the text as written.
    pub birthday: Option<String>,
    /// Every `NOTE`, joined by an empty line.
    pub note: Option<String>,
    /// The first `PHOTO`.
    pub photo: Option<Photo>,
}

/// The components of `N`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameParts {
    /// Family name.
    pub family: Option<String>,
    /// Given name.
    pub given: Option<String>,
    /// Additional names.
    pub additional: Option<String>,
    /// Honorific prefixes.
    pub prefix: Option<String>,
    /// Honorific suffixes.
    pub suffix: Option<String>,
}

/// An email address of a contact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Email {
    /// The address as written.
    pub address: String,
    /// The address for matching: see [`normalize_email`]. `None` when it is not an address.
    pub normalized: Option<String>,
    /// `TYPE` values, lowercase, such as `work` or `home`.
    pub kinds: Vec<String>,
}

/// A phone number of a contact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phone {
    /// The number as written.
    pub number: String,
    /// The number in E.164 form when it can be known: see [`normalize_phone`].
    pub e164: Option<String>,
    /// `TYPE` values, lowercase, such as `cell` or `work`.
    pub kinds: Vec<String>,
}

/// A postal address of a contact; every part is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostalAddress {
    /// `TYPE` values, lowercase.
    pub kinds: Vec<String>,
    /// Post office box.
    pub po_box: Option<String>,
    /// Extended address, such as an apartment.
    pub extended: Option<String>,
    /// Street.
    pub street: Option<String>,
    /// City.
    pub locality: Option<String>,
    /// Region or state.
    pub region: Option<String>,
    /// Postal code.
    pub postal_code: Option<String>,
    /// Country.
    pub country: Option<String>,
}

/// A contact's picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Photo {
    /// Bytes inside the vCard, kept as a blob of their own with this id.
    Embedded {
        /// The id of the picture's bytes.
        blob: BlobId,
        /// The picture's media type, from its first bytes or the vCard.
        content_type: Option<String>,
        /// Its size in bytes.
        size: u64,
    },
    /// A link to a picture elsewhere, never fetched.
    Link {
        /// The address.
        url: String,
    },
}

/// The bytes of a picture embedded in a vCard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedPhoto {
    /// The decoded bytes.
    pub bytes: Vec<u8>,
    /// The media type, from the bytes or the vCard.
    pub content_type: Option<String>,
}

pub(crate) fn parse(raw: &[u8]) -> Option<(Contact, Option<EmbeddedPhoto>)> {
    let text = String::from_utf8_lossy(raw);
    let card = VCard::parse(text.as_ref()).ok()?;
    Some(from_card(&card))
}

pub(crate) fn from_card(card: &VCard) -> (Contact, Option<EmbeddedPhoto>) {
    let mut contact = Contact {
        uid: text_of(card, &VCardProperty::Uid),
        rev: card
            .property(&VCardProperty::Rev)
            .and_then(|e| e.values.first())
            .and_then(date_text),
        full_name: text_of(card, &VCardProperty::Fn),
        name: card.property(&VCardProperty::N).and_then(name_parts),
        nicknames: card
            .properties(&VCardProperty::Nickname)
            .flat_map(texts)
            .collect(),
        organization: card.property(&VCardProperty::Org).and_then(|e| {
            let units = texts(e);
            (!units.is_empty()).then(|| units.join(", "))
        }),
        title: text_of(card, &VCardProperty::Title),
        birthday: card
            .property(&VCardProperty::Bday)
            .and_then(|e| e.values.first())
            .and_then(date_text),
        ..Contact::default()
    };
    for entry in card.properties(&VCardProperty::Email) {
        if let Some(address) = joined(entry) {
            contact.emails.push(Email {
                normalized: normalize_email(&address),
                address,
                kinds: kinds(entry),
            });
        }
    }
    for entry in card.properties(&VCardProperty::Tel) {
        if let Some(number) = joined(entry) {
            contact.phones.push(Phone {
                e164: normalize_phone(&number),
                number,
                kinds: kinds(entry),
            });
        }
    }
    for entry in card.properties(&VCardProperty::Adr) {
        let parts = components(entry);
        let part = |i: usize| parts.get(i).cloned().flatten();
        let address = PostalAddress {
            kinds: kinds(entry),
            po_box: part(0),
            extended: part(1),
            street: part(2),
            locality: part(3),
            region: part(4),
            postal_code: part(5),
            country: part(6),
        };
        if address
            != (PostalAddress {
                kinds: address.kinds.clone(),
                ..PostalAddress::default()
            })
        {
            contact.addresses.push(address);
        }
    }
    let notes: Vec<String> = card
        .properties(&VCardProperty::Note)
        .filter_map(joined)
        .collect();
    contact.note = (!notes.is_empty()).then(|| notes.join("\n\n"));
    let mut embedded = None;
    if let Some(entry) = card.property(&VCardProperty::Photo) {
        match photo(entry) {
            Some(PhotoSource::Bytes(found)) => {
                contact.photo = Some(Photo::Embedded {
                    blob: BlobId::of(&found.bytes),
                    content_type: found.content_type.clone(),
                    size: found.bytes.len() as u64,
                });
                embedded = Some(found);
            }
            Some(PhotoSource::Link(url)) => contact.photo = Some(Photo::Link { url }),
            None => {}
        }
    }
    (contact, embedded)
}

fn clean(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

fn value_text(value: &VCardValue) -> Option<String> {
    match value {
        VCardValue::Component(parts) => clean(&parts.join(", ")),
        VCardValue::PartialDateTime(date) => format_partial(date),
        VCardValue::Integer(n) => Some(n.to_string()),
        other => other.as_text().and_then(clean),
    }
}

fn texts(entry: &VCardEntry) -> Vec<String> {
    entry.values.iter().filter_map(value_text).collect()
}

fn joined(entry: &VCardEntry) -> Option<String> {
    let parts = texts(entry);
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn components(entry: &VCardEntry) -> Vec<Option<String>> {
    entry.values.iter().map(value_text).collect()
}

fn text_of(card: &VCard, property: &VCardProperty) -> Option<String> {
    card.property(property).and_then(joined)
}

fn name_parts(entry: &VCardEntry) -> Option<NameParts> {
    let parts = components(entry);
    let part = |i: usize| parts.get(i).cloned().flatten();
    let name = NameParts {
        family: part(0),
        given: part(1),
        additional: part(2),
        prefix: part(3),
        suffix: part(4),
    };
    (name != NameParts::default()).then_some(name)
}

fn kinds(entry: &VCardEntry) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in entry.parameters(&VCardParameterName::Type) {
        let text = match value.as_type() {
            Some(IanaType::Iana(kind)) => kind.as_str().to_owned(),
            Some(IanaType::Other(text)) => text.to_owned(),
            None => match value {
                VCardParameterValue::Text(text) => text.clone(),
                _ => continue,
            },
        };
        for kind in text.split(',') {
            let kind = kind.trim().to_ascii_lowercase();
            if !kind.is_empty() && kind != "internet" && kind != "pref" && !out.contains(&kind) {
                out.push(kind);
            }
        }
    }
    out
}

fn date_text(value: &VCardValue) -> Option<String> {
    value_text(value)
}

pub(crate) fn format_partial(date: &PartialDateTime) -> Option<String> {
    let mut out = String::new();
    match (date.year, date.month, date.day) {
        (Some(y), Some(m), Some(d)) => out.push_str(&format!("{y:04}-{m:02}-{d:02}")),
        (None, Some(m), Some(d)) => out.push_str(&format!("--{m:02}-{d:02}")),
        (Some(y), Some(m), None) => out.push_str(&format!("{y:04}-{m:02}")),
        (Some(y), None, None) => out.push_str(&format!("{y:04}")),
        _ => {}
    }
    if let (Some(h), Some(mi)) = (date.hour, date.minute) {
        out.push_str(&format!(
            "T{h:02}:{mi:02}:{:02}",
            date.second.unwrap_or_default()
        ));
        if let Some(tz) = date.tz_hour {
            let minutes = date.tz_minute.unwrap_or_default();
            if tz == 0 && minutes == 0 {
                out.push('Z');
            } else {
                let sign = if date.tz_minus { '-' } else { '+' };
                out.push_str(&format!("{sign}{tz:02}:{minutes:02}"));
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

enum PhotoSource {
    Bytes(EmbeddedPhoto),
    Link(String),
}

fn photo(entry: &VCardEntry) -> Option<PhotoSource> {
    let declared = entry
        .parameters(&VCardParameterName::Type)
        .chain(entry.parameters(&VCardParameterName::Mediatype))
        .find_map(|v| match v.as_type() {
            Some(IanaType::Other(text)) => Some(text.to_owned()),
            _ => v.as_text().map(str::to_owned),
        })
        .map(|t| media_type(&t));
    match entry.values.first()? {
        VCardValue::Binary(data) => Some(PhotoSource::Bytes(EmbeddedPhoto {
            content_type: sniff(&data.data).or(data.content_type.clone()).or(declared),
            bytes: data.data.clone(),
        })),
        value => {
            let text = value.as_text()?.trim();
            if text.is_empty() {
                return None;
            }
            if let Some(rest) = strip_prefix_ignore_case(text, "data:") {
                let (meta, payload) = rest.split_once(',')?;
                let bytes = if meta.to_ascii_lowercase().ends_with(";base64") {
                    use base64::Engine;
                    let compact: String = payload.split_whitespace().collect();
                    base64::engine::general_purpose::STANDARD
                        .decode(compact)
                        .ok()?
                } else {
                    payload.as_bytes().to_vec()
                };
                let meta_type = meta.split(';').next().filter(|t| !t.is_empty());
                return Some(PhotoSource::Bytes(EmbeddedPhoto {
                    content_type: sniff(&bytes).or(meta_type.map(str::to_owned)).or(declared),
                    bytes,
                }));
            }
            let looks_like_link = text.split_once(':').is_some_and(|(scheme, _)| {
                !scheme.is_empty()
                    && scheme
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
            });
            if looks_like_link {
                Some(PhotoSource::Link(text.to_owned()))
            } else {
                let bytes = value.as_text()?.as_bytes().to_vec();
                Some(PhotoSource::Bytes(EmbeddedPhoto {
                    content_type: sniff(&bytes).or(declared),
                    bytes,
                }))
            }
        }
    }
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
        .then(|| &text[prefix.len()..])
}

fn media_type(declared: &str) -> String {
    let lower = declared.trim().to_ascii_lowercase();
    if lower.contains('/') {
        return lower;
    }
    match lower.as_str() {
        "jpg" | "jpeg" => "image/jpeg".into(),
        other => format!("image/{other}"),
    }
}

fn sniff(bytes: &[u8]) -> Option<String> {
    let kind = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        return None;
    };
    Some(kind.to_owned())
}

impl Contact {
    /// The name to show: `FN`, else the parts of `N`, else the organization, else the first
    /// email address or phone number.
    pub fn display_name(&self) -> Option<String> {
        self.full_name
            .clone()
            .or_else(|| self.name.as_ref().map(NameParts::formatted))
            .filter(|n| !n.is_empty())
            .or_else(|| self.organization.clone())
            .or_else(|| self.emails.first().map(|e| e.address.clone()))
            .or_else(|| self.phones.first().map(|p| p.number.clone()))
    }
}

impl NameParts {
    /// Prefix, given, additional and family names and the suffix, separated by spaces.
    pub fn formatted(&self) -> String {
        [
            &self.prefix,
            &self.given,
            &self.additional,
            &self.family,
            &self.suffix,
        ]
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
    }
}

impl PostalAddress {
    /// Every part that is set, on one line separated by `, `.
    pub fn formatted(&self) -> String {
        [
            &self.po_box,
            &self.extended,
            &self.street,
            &self.locality,
            &self.region,
            &self.postal_code,
            &self.country,
        ]
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(", ")
    }
}
