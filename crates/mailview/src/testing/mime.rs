use holtmail_core::{Address, Timestamp};

use crate::{ByteRange, Part};

pub(crate) type Fields = Vec<(String, String)>;

fn find(raw: &[u8], from: usize, end: usize, needle: &[u8]) -> Option<usize> {
    if end < from + needle.len() {
        return None;
    }
    (from..=end - needle.len()).find(|&i| &raw[i..i + needle.len()] == needle)
}

pub(crate) fn header_end(raw: &[u8], start: usize, end: usize) -> usize {
    if raw[start..end].starts_with(b"\r\n") {
        return start + 2;
    }
    find(raw, start, end, b"\r\n\r\n").map_or(end, |i| i + 4)
}

pub(crate) fn fields(raw: &[u8]) -> Fields {
    let text = String::from_utf8_lossy(raw);
    let mut out: Fields = Vec::new();
    for line in text.split("\r\n") {
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

pub(crate) fn angle_id(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    let start = value.find('<')?;
    let end = value[start..].find('>')? + start;
    Some(value[start + 1..end].to_string())
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub(crate) fn date(value: Option<&str>) -> Option<Timestamp> {
    let value = value?;
    let value = value.split_once(',').map_or(value, |(_, rest)| rest);
    let parts: Vec<&str> = value.split_whitespace().collect();
    let [day, month, year, time, zone, ..] = parts.as_slice() else {
        return None;
    };
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let month = MONTHS.iter().position(|m| m.eq_ignore_ascii_case(month))? as i64 + 1;
    let mut hms = time.split(':').map(|p| p.parse::<i64>());
    let (h, mi, s) = (
        hms.next()?.ok()?,
        hms.next()?.ok()?,
        hms.next().unwrap_or(Ok(0)).ok()?,
    );
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let digits: i64 = zone.trim_start_matches(['+', '-']).parse().ok()?;
    let offset = sign * ((digits / 100) * 3600 + (digits % 100) * 60);
    let days = days_from_civil(year.parse().ok()?, month, day.parse().ok()?);
    Some(Timestamp::from_secs(
        days * 86_400 + h * 3600 + mi * 60 + s - offset,
    ))
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

pub(crate) fn structure(raw: &[u8]) -> Part {
    part(raw, 0, raw.len(), String::new())
}

fn part(raw: &[u8], start: usize, end: usize, id: String) -> Part {
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
    let body = ByteRange {
        start: header_end as u64,
        end: end as u64,
    };
    let mut parts = Vec::new();
    if content_type.starts_with("multipart/") {
        if let Some((_, boundary)) = params.iter().find(|(n, _)| n == "boundary") {
            let delimiter = format!("--{boundary}");
            let mut cursor = header_end;
            let mut open: Option<usize> = None;
            let mut index = 0;
            while let Some(at) = find(raw, cursor, end, delimiter.as_bytes()) {
                let line_start = at == header_end || (at >= 2 && &raw[at - 2..at] == b"\r\n");
                if !line_start {
                    cursor = at + 1;
                    continue;
                }
                let close = if at == header_end { at } else { at - 2 };
                if let Some(from) = open {
                    index += 1;
                    let child = if id.is_empty() {
                        index.to_string()
                    } else {
                        format!("{id}.{index}")
                    };
                    parts.push(part(raw, from, close, child));
                }
                let after = at + delimiter.len();
                if raw[after..end].starts_with(b"--") {
                    break;
                }
                let next = find(raw, after, end, b"\r\n").map_or(end, |i| i + 2);
                open = Some(next);
                cursor = next;
            }
        }
    } else if content_type == "message/rfc822" {
        parts.push(part(raw, header_end, end, id.clone()));
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
        body,
        lines: raw[header_end..end].iter().filter(|b| **b == b'\n').count() as u64,
        parts,
    }
}

pub(crate) fn has_attachment(part: &Part) -> bool {
    part.disposition.as_deref() == Some("attachment") || part.parts.iter().any(has_attachment)
}

pub(crate) fn first_text(raw: &[u8], part: &Part) -> Option<String> {
    if part.parts.is_empty() {
        return (part.content_type == "text/plain").then(|| {
            String::from_utf8_lossy(&raw[part.body.start as usize..part.body.end as usize])
                .into_owned()
        });
    }
    part.parts.iter().find_map(|p| first_text(raw, p))
}

pub(crate) fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(
            date(Some("Sun, 08 Nov 2026 10:00:00 +0100")),
            Some(Timestamp::from_secs(1_794_128_400))
        );
        assert_eq!(date(Some("nonsense")), None);
    }
}
