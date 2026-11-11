use holtmail_core::Address;
use holtmail_mailview::{ByteRange, Part};

pub(crate) type Fields = Vec<(String, String)>;

fn find(raw: &[u8], from: usize, end: usize, needle: &[u8]) -> Option<usize> {
    if end < from + needle.len() {
        return None;
    }
    (from..=end - needle.len()).find(|&i| &raw[i..i + needle.len()] == needle)
}

fn line_end(raw: &[u8], from: usize, end: usize) -> usize {
    find(raw, from, end, b"\n").map_or(end, |i| i + 1)
}

/// Where the header that starts at `start` ends, through its empty line; bare LF line ends
/// count as well as CRLF.
pub(crate) fn header_end(raw: &[u8], start: usize, end: usize) -> usize {
    let mut at = start;
    while at < end {
        let next = line_end(raw, at, end);
        let line = &raw[at..next];
        if line == b"\r\n" || line == b"\n" {
            return next;
        }
        at = next;
    }
    end
}

pub(crate) fn fields(raw: &[u8]) -> Fields {
    let text = String::from_utf8_lossy(raw);
    let mut out: Fields = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = out.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            out.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    out
}

pub(crate) fn get<'a>(fields: &'a Fields, name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

pub(crate) fn top_fields(raw: &[u8]) -> Fields {
    fields(&raw[..header_end(raw, 0, raw.len())])
}

pub(crate) fn addresses(value: Option<&str>) -> Vec<Address> {
    let Some(value) = value else {
        return Vec::new();
    };
    value
        .split(',')
        .filter_map(|one| {
            let one = one.trim();
            if one.is_empty() {
                return None;
            }
            Some(match (one.find('<'), one.rfind('>')) {
                (Some(open), Some(close)) if open < close => {
                    let name = one[..open].trim().trim_matches('"').trim();
                    Address {
                        name: (!name.is_empty()).then(|| name.to_string()),
                        email: one[open + 1..close].trim().to_string(),
                    }
                }
                _ => Address {
                    name: None,
                    email: one.to_string(),
                },
            })
        })
        .collect()
}

fn params(value: &str) -> (String, Vec<(String, String)>) {
    let mut pieces = value.split(';');
    let head = pieces
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let params = pieces
        .filter_map(|p| {
            let (name, value) = p.split_once('=')?;
            Some((
                name.trim().to_ascii_lowercase(),
                value.trim().trim_matches('"').to_string(),
            ))
        })
        .collect();
    (head, params)
}

/// The MIME structure of a message with byte ranges into `raw`.
pub(crate) fn structure(raw: &[u8]) -> Part {
    part(raw, 0, raw.len(), String::new(), 0)
}

const MAX_DEPTH: usize = 32;

fn part(raw: &[u8], start: usize, end: usize, id: String, depth: usize) -> Part {
    let header_end = header_end(raw, start, end);
    let fields = fields(&raw[start..header_end]);
    let (content_type, params) = params(get(&fields, "content-type").unwrap_or("text/plain"));
    let (disposition, disposition_params) = match get(&fields, "content-disposition") {
        Some(value) => {
            let (d, p) = self::params(value);
            (Some(d), p)
        }
        None => (None, Vec::new()),
    };
    let mut parts = Vec::new();
    if depth < MAX_DEPTH && content_type.starts_with("multipart/") {
        if let Some((_, boundary)) = params.iter().find(|(n, _)| n == "boundary") {
            parts = children(raw, header_end, end, boundary, &id, depth);
        }
    } else if depth < MAX_DEPTH && content_type == "message/rfc822" {
        parts.push(part(raw, header_end, end, id.clone(), depth + 1));
    }
    Part {
        id,
        content_type: if content_type.contains('/') {
            content_type
        } else {
            "text/plain".into()
        },
        params,
        disposition,
        disposition_params,
        content_id: get(&fields, "content-id").map(str::to_string),
        description: get(&fields, "content-description").map(str::to_string),
        transfer_encoding: get(&fields, "content-transfer-encoding")
            .map_or_else(|| "7bit".into(), |v| v.trim().to_ascii_lowercase()),
        language: get(&fields, "content-language")
            .map(|v| v.split(',').map(|l| l.trim().to_string()).collect())
            .unwrap_or_default(),
        location: get(&fields, "content-location").map(str::to_string),
        header: ByteRange {
            start: start as u64,
            end: header_end as u64,
        },
        body: ByteRange {
            start: header_end as u64,
            end: end as u64,
        },
        lines: raw[header_end..end].iter().filter(|b| **b == b'\n').count() as u64,
        parts,
    }
}

fn children(
    raw: &[u8],
    body: usize,
    end: usize,
    boundary: &str,
    id: &str,
    depth: usize,
) -> Vec<Part> {
    let delimiter = format!("--{boundary}");
    let mut parts = Vec::new();
    let mut cursor = body;
    let mut open: Option<usize> = None;
    let mut index = 0;
    while let Some(at) = find(raw, cursor, end, delimiter.as_bytes()) {
        let line_start = at == body || raw[at - 1] == b'\n';
        if !line_start {
            cursor = at + 1;
            continue;
        }
        let close = if at == body {
            at
        } else if at >= 2 && raw[at - 2] == b'\r' {
            at - 2
        } else {
            at - 1
        };
        if let Some(from) = open {
            index += 1;
            let child = if id.is_empty() {
                index.to_string()
            } else {
                format!("{id}.{index}")
            };
            parts.push(part(raw, from, close.max(from), child, depth + 1));
        }
        let after = at + delimiter.len();
        if raw[after..end].starts_with(b"--") {
            break;
        }
        let next = line_end(raw, after, end);
        open = Some(next);
        cursor = next;
    }
    parts
}

pub(crate) fn has_attachment(part: &Part) -> bool {
    part.disposition.as_deref() == Some("attachment") || part.parts.iter().any(has_attachment)
}

pub(crate) fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Bytes of `raw` with every bare LF counted as CRLF.
pub(crate) fn crlf_size(raw: &[u8]) -> u64 {
    let bare = raw
        .iter()
        .enumerate()
        .filter(|(i, b)| **b == b'\n' && (*i == 0 || raw[i - 1] != b'\r'))
        .count();
    (raw.len() + bare) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_with_bare_lf() {
        let raw = b"Content-Type: multipart/mixed; boundary=x\n\n--x\nContent-Type: text/plain\n\nHello\n--x--\n";
        let part = structure(raw);
        assert_eq!(part.parts.len(), 1);
        let text = &part.parts[0];
        assert_eq!(
            &raw[text.body.start as usize..text.body.end as usize],
            b"Hello"
        );
        assert_eq!(crlf_size(raw), raw.len() as u64 + 7);
    }
}
