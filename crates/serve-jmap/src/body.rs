use holtmail_mailview::{ByteRange, EmailRef, Part};
use mail_parser::{Encoding, HeaderForm};
use serde_json::{Map, Value, json};

use crate::ids;

pub(crate) const DEFAULT_BODY_PROPERTIES: &[&str] = &[
    "partId",
    "blobId",
    "size",
    "name",
    "type",
    "charset",
    "disposition",
    "cid",
    "language",
    "location",
];

fn slice(raw: &[u8], range: ByteRange) -> &[u8] {
    let end = (range.end as usize).min(raw.len());
    let start = (range.start as usize).min(end);
    &raw[start..end]
}

/// The JMAP part id of a part: the IMAP section, `0` for the whole message.
pub(crate) fn part_id(part: &Part) -> String {
    if part.id.is_empty() {
        "0".into()
    } else {
        part.id.clone()
    }
}

/// The part with this JMAP part id.
pub(crate) fn find<'p>(root: &'p Part, id: &str) -> Option<&'p Part> {
    let section = if id == "0" { "" } else { id };
    fn go<'p>(part: &'p Part, section: &str) -> Option<&'p Part> {
        if part.id == section && !is_multipart(part) {
            return Some(part);
        }
        if is_message(part) {
            return None;
        }
        part.parts.iter().find_map(|child| go(child, section))
    }
    go(root, section)
}

fn is_multipart(part: &Part) -> bool {
    part.content_type.starts_with("multipart/")
}

fn is_message(part: &Part) -> bool {
    part.content_type == "message/rfc822" || part.content_type == "message/global"
}

fn param<'p>(params: &'p [(String, String)], name: &str) -> Option<&'p str> {
    params
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

pub(crate) fn name(part: &Part) -> Option<&str> {
    param(&part.disposition_params, "filename").or_else(|| param(&part.params, "name"))
}

fn encoding(part: &Part) -> Encoding {
    match part.transfer_encoding.as_str() {
        "base64" => Encoding::Base64,
        "quoted-printable" => Encoding::QuotedPrintable,
        _ => Encoding::None,
    }
}

/// The part's body with its transfer encoding undone.
pub(crate) fn decoded(raw: &[u8], part: &Part) -> Vec<u8> {
    encoding(part).decode(slice(raw, part.body)).into_owned()
}

fn charset(part: &Part) -> Option<String> {
    param(&part.params, "charset")
        .map(str::to_string)
        .or_else(|| {
            part.content_type
                .starts_with("text/")
                .then(|| "us-ascii".into())
        })
}

/// The part's text, decoded from its charset.
pub(crate) fn text(raw: &[u8], part: &Part) -> String {
    let bytes = decoded(raw, part);
    let label = charset(part).unwrap_or_else(|| "utf-8".into());
    let mut out = String::new();
    let label = if label.eq_ignore_ascii_case("us-ascii") {
        "utf-8".to_string()
    } else {
        label
    };
    mail_parser::decoders::charsets::decode_append(label.as_bytes(), &bytes, &mut out);
    out
}

/// The header fields of a header block, unfolded only by the caller's form: name and the raw
/// value after the colon.
pub(crate) fn fields(block: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(block);
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']).is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some(last) = out.last_mut() {
                last.1.push_str(line);
            }
        } else if let Some((name, value)) = line.split_once(':') {
            out.push((name.trim().to_string(), value.to_string()));
        }
    }
    for field in &mut out {
        let trimmed = field.1.trim_end_matches(['\r', '\n']).len();
        field.1.truncate(trimmed);
    }
    out
}

/// A requested `header:` property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeaderProperty {
    pub name: String,
    pub form: Form,
    pub all: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Form {
    Raw,
    Text,
    Addresses,
    GroupedAddresses,
    MessageIds,
    Date,
    Urls,
}

impl HeaderProperty {
    pub fn parse(property: &str) -> Option<Self> {
        let rest = property.strip_prefix("header:")?;
        let mut pieces: Vec<&str> = rest.split(':').collect();
        let all = pieces.last() == Some(&"all");
        if all {
            pieces.pop();
        }
        let form = match pieces.len() {
            1 => Form::Raw,
            2 => match pieces[1] {
                "asRaw" => Form::Raw,
                "asText" => Form::Text,
                "asAddresses" => Form::Addresses,
                "asGroupedAddresses" => Form::GroupedAddresses,
                "asMessageIds" => Form::MessageIds,
                "asDate" => Form::Date,
                "asURLs" => Form::Urls,
                _ => return None,
            },
            _ => return None,
        };
        let name = pieces[0];
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_graphic() && b != b':') {
            return None;
        }
        Some(Self {
            name: name.to_string(),
            form,
            all,
        })
    }

    pub fn value(&self, fields: &[(String, String)]) -> Value {
        let values: Vec<Value> = fields
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(&self.name))
            .map(|(_, v)| form_value(self.form, v))
            .collect();
        if self.all {
            Value::Array(values)
        } else {
            values.into_iter().last().unwrap_or(Value::Null)
        }
    }
}

fn addresses(value: &str) -> Vec<(Option<String>, Vec<Value>)> {
    let mut bytes = value.as_bytes().to_vec();
    bytes.extend_from_slice(b"\r\n");
    let parsed = HeaderForm::Addresses.parse(&bytes);
    let Some(list) = parsed.value().as_address() else {
        return Vec::new();
    };
    list.groups()
        .map(|(name, run)| {
            let members = run
                .map(|m| json!({"name": m.name(), "email": m.address().unwrap_or_default()}))
                .collect();
            (name.map(str::to_string), members)
        })
        .collect()
}

pub(crate) fn address_list(value: &str) -> Value {
    Value::Array(addresses(value).into_iter().flat_map(|(_, m)| m).collect())
}

fn form_value(form: Form, value: &str) -> Value {
    let mut bytes = value.as_bytes().to_vec();
    bytes.extend_from_slice(b"\r\n");
    match form {
        Form::Raw => json!(value),
        Form::Text => {
            let parsed = HeaderForm::Text.parse(&bytes);
            let text = parsed.value().as_text().map(str::to_string);
            json!(text.unwrap_or_else(|| value.trim().to_string()))
        }
        Form::Addresses => address_list(value),
        Form::GroupedAddresses => Value::Array(
            addresses(value)
                .into_iter()
                .map(|(name, addresses)| json!({"name": name, "addresses": addresses}))
                .collect(),
        ),
        Form::MessageIds => {
            let parsed = HeaderForm::MessageIds.parse(&bytes);
            match parsed.value().as_text_list() {
                Some(list) if !list.is_empty() => json!(list.iter().collect::<Vec<_>>()),
                _ => match parsed.value().as_text() {
                    Some(one) => json!([one]),
                    None => Value::Null,
                },
            }
        }
        Form::Date => {
            let parsed = HeaderForm::Date.parse(&bytes);
            match parsed.value().as_datetime() {
                Some(date) => json!(crate::time::utc_date(date.to_timestamp() * 1000)),
                None => Value::Null,
            }
        }
        Form::Urls => {
            let parsed = HeaderForm::CommaList.parse(&bytes);
            let urls: Vec<String> = parsed
                .value()
                .as_text_list()
                .map(|list| {
                    list.iter()
                        .map(|t| {
                            t.trim()
                                .trim_start_matches('<')
                                .trim_end_matches('>')
                                .into()
                        })
                        .collect()
                })
                .unwrap_or_default();
            if urls.is_empty() {
                Value::Null
            } else {
                json!(urls)
            }
        }
    }
}

/// A body part as JMAP `EmailBodyPart`, with only the asked properties.
pub(crate) fn part_json(email: &EmailRef, raw: &[u8], part: &Part, properties: &[String]) -> Value {
    let multipart = is_multipart(part);
    let mut out = Map::new();
    let header_fields = fields(slice(raw, part.header));
    for property in properties {
        let value = match property.as_str() {
            "partId" => match multipart {
                true => Value::Null,
                false => json!(part_id(part)),
            },
            "blobId" => match multipart {
                true => Value::Null,
                false => json!(ids::part_blob(email, &part_id(part))),
            },
            "size" => match multipart {
                true => json!(0),
                false => json!(decoded(raw, part).len()),
            },
            "headers" => Value::Array(
                header_fields
                    .iter()
                    .map(|(n, v)| json!({"name": n, "value": v}))
                    .collect(),
            ),
            "name" => json!(name(part)),
            "type" => json!(part.content_type),
            "charset" => match multipart {
                true => Value::Null,
                false => json!(charset(part)),
            },
            "disposition" => json!(part.disposition),
            "cid" => json!(
                part.content_id
                    .as_deref()
                    .map(|c| c.trim().trim_start_matches('<').trim_end_matches('>'))
            ),
            "language" => match part.language.is_empty() {
                true => Value::Null,
                false => json!(part.language),
            },
            "location" => json!(part.location),
            "subParts" => match multipart {
                true => Value::Array(
                    part.parts
                        .iter()
                        .map(|p| part_json(email, raw, p, properties))
                        .collect(),
                ),
                false => Value::Null,
            },
            other => match HeaderProperty::parse(other) {
                Some(header) => header.value(&header_fields),
                None => continue,
            },
        };
        out.insert(property.clone(), value);
    }
    Value::Object(out)
}

/// The RFC 8621 §4.1.4 body lists, as references to leaf parts.
pub(crate) struct Lists<'p> {
    pub text: Vec<&'p Part>,
    pub html: Vec<&'p Part>,
    pub attachments: Vec<&'p Part>,
}

fn inline_media(part: &Part) -> bool {
    ["image/", "audio/", "video/"]
        .iter()
        .any(|p| part.content_type.starts_with(p))
}

pub(crate) fn lists(root: &Part) -> Lists<'_> {
    let mut lists = Lists {
        text: Vec::new(),
        html: Vec::new(),
        attachments: Vec::new(),
    };
    walk_lists(&[root], "mixed", false, &mut lists, true, true);
    lists
}

fn walk_lists<'p>(
    parts: &[&'p Part],
    multipart_type: &str,
    in_alternative: bool,
    lists: &mut Lists<'p>,
    mut text_on: bool,
    mut html_on: bool,
) {
    let text_length = lists.text.len();
    let html_length = lists.html.len();
    for (i, part) in parts.iter().copied().enumerate() {
        let kind = part.content_type.as_str();
        let inline = part.disposition.as_deref() != Some("attachment")
            && (kind == "text/plain" || kind == "text/html" || inline_media(part))
            && (i == 0
                || (multipart_type != "related" && (inline_media(part) || name(part).is_none())));
        if is_multipart(part) {
            let sub = kind.split_once('/').map(|(_, s)| s).unwrap_or("mixed");
            let children: Vec<&Part> = part.parts.iter().collect();
            walk_lists(
                &children,
                sub,
                in_alternative || sub == "alternative",
                lists,
                text_on,
                html_on,
            );
        } else if inline {
            if multipart_type == "alternative" {
                match kind {
                    "text/plain" => lists.text.push(part),
                    "text/html" => lists.html.push(part),
                    _ => lists.attachments.push(part),
                }
                continue;
            } else if in_alternative {
                if kind == "text/plain" {
                    html_on = false;
                }
                if kind == "text/html" {
                    text_on = false;
                }
            }
            if text_on {
                lists.text.push(part);
            }
            if html_on {
                lists.html.push(part);
            }
            if (!text_on || !html_on) && inline_media(part) {
                lists.attachments.push(part);
            }
        } else {
            lists.attachments.push(part);
        }
    }
    if multipart_type == "alternative" && text_on && html_on {
        if text_length == lists.text.len() && html_length != lists.html.len() {
            let extra: Vec<&Part> = lists.html[html_length..].to_vec();
            lists.text.extend(extra);
        }
        if html_length == lists.html.len() && text_length != lists.text.len() {
            let extra: Vec<&Part> = lists.text[text_length..].to_vec();
            lists.html.extend(extra);
        }
    }
}

/// A body value: the decoded text, cut to at most `max` bytes at a character boundary.
pub(crate) fn body_value(raw: &[u8], part: &Part, max: Option<u64>) -> Value {
    let mut value = text(raw, part);
    let problem = value.contains('\u{FFFD}');
    let mut truncated = false;
    if let Some(max) = max.filter(|m| *m > 0) {
        let max = max as usize;
        if value.len() > max {
            let mut cut = max;
            while !value.is_char_boundary(cut) {
                cut -= 1;
            }
            value.truncate(cut);
            truncated = true;
        }
    }
    json!({"value": value, "isEncodingProblem": problem, "isTruncated": truncated})
}

/// The first text the reader sees, for snippets: the first text body part, as plain text.
pub(crate) fn plain_text(raw: &[u8], root: &Part) -> String {
    let lists = lists(root);
    if let Some(part) = lists.text.iter().find(|p| p.content_type == "text/plain") {
        return text(raw, part);
    }
    if let Some(part) = lists.html.first() {
        return mail_parser::html_to_text(&text(raw, part));
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_properties_parse() {
        assert_eq!(
            HeaderProperty::parse("header:List-Id:asText:all"),
            Some(HeaderProperty {
                name: "List-Id".into(),
                form: Form::Text,
                all: true
            })
        );
        assert_eq!(
            HeaderProperty::parse("header:From").map(|h| h.form),
            Some(Form::Raw)
        );
        assert!(HeaderProperty::parse("header:From:asNope").is_none());
        assert!(HeaderProperty::parse("header:").is_none());
    }

    #[test]
    fn header_forms() {
        let block = b"From: \"Ada L.\" <ada@example.com>\r\nSubject: =?utf-8?q?Caf=C3=A9?=\r\n\
            References: <a@example.com>\r\n <b@example.com>\r\nList-Post: <mailto:l@example.org>\r\n\r\nbody";
        let fields = fields(block);
        assert_eq!(fields.len(), 4);
        let get = |p: &str| HeaderProperty::parse(p).unwrap().value(&fields);
        assert_eq!(get("header:Subject:asText"), json!("Café"));
        assert_eq!(
            get("header:From:asAddresses"),
            json!([{"name": "Ada L.", "email": "ada@example.com"}])
        );
        assert_eq!(
            get("header:References:asMessageIds"),
            json!(["a@example.com", "b@example.com"])
        );
        assert_eq!(
            get("header:List-Post:asURLs"),
            json!(["mailto:l@example.org"])
        );
        assert_eq!(get("header:subject"), json!(" =?utf-8?q?Caf=C3=A9?="));
        assert_eq!(get("header:X-Missing:all"), json!([]));
    }
}
