use std::io::{Read, Write};

use flate2::{Compression, GzBuilder, read::GzDecoder};
use holtmail_category::{Topic, TopicSet};

use crate::{
    DataError, DomainTable, FORMAT, Header, Limits, Row, is_valid_domain, is_valid_name,
    is_valid_source, is_valid_version,
};

const MAGIC: &str = "#holtmail-categories";

/// Reads a whole file with its `source` column, for tools that rewrite or compare files.
///
/// Applies exactly the checks of [`parse_with`] under `limits`. Rows are returned in file
/// order, which is byte order of the domain; rows whose topics are all unknown to this build
/// are left out, and unknown topic names are dropped from the others.
pub fn read(gz: &[u8], limits: Limits) -> Result<(Header, Vec<Row>), DataError> {
    let text = decompress(gz, limits)?;
    let mut rows = Vec::new();
    let (header, _) = scan(&text, limits, |domain, topics, source| {
        rows.push(Row {
            domain: domain.to_owned(),
            topics,
            source: source.to_owned(),
        });
    })?;
    Ok((header, rows))
}

/// Loads a file under the default [`Limits`]. See [`parse_with`].
pub fn parse(gz: &[u8]) -> Result<DomainTable, DataError> {
    parse_with(gz, Limits::default())
}

/// Loads a file into a [`DomainTable`] under `limits`.
///
/// Decompression stops one byte past `limits.max_bytes`, so a gzip bomb is refused as
/// [`DataError::TooLarge`] without allocating past the cap. The whole file is refused, never
/// partly loaded, when it has more than `limits.max_rows` data rows, invalid UTF-8, a bad
/// header, a line not ending in a newline, a line without exactly three tab-separated
/// columns, an invalid domain (see [`crate::is_valid_domain`]), an empty or malformed topic
/// list, an invalid source, or a domain not strictly greater in byte order than the one
/// before it. Line errors carry the 1-based line number.
///
/// A topic name this build does not know is skipped, so a newer taxonomy still loads; a row
/// left with no known topic is dropped and counted in [`DomainTable::skipped`].
pub fn parse_with(gz: &[u8], limits: Limits) -> Result<DomainTable, DataError> {
    let text = decompress(gz, limits)?;
    let mut rows: Vec<(Box<str>, TopicSet)> = Vec::new();
    let (header, skipped) = scan(&text, limits, |domain, topics, _| {
        rows.push((domain.into(), topics));
    })?;
    rows.shrink_to_fit();
    let hash = blake3::hash(&text).to_hex().to_string();
    Ok(DomainTable::new(header, rows, skipped, hash))
}

/// Writes a file: the header line, then one line per row, gzip-compressed.
///
/// The header, every domain and every source are checked as [`parse_with`] checks them.
/// Rows must be strictly increasing in byte order of the domain, and every row's topics must
/// be non-empty and known to this build; otherwise [`DataError::Header`] or
/// [`DataError::Line`] names the rule and the line the row would have had. Topics are written
/// by name in bit order. The gzip stream has no file name and a zero timestamp and uses the
/// best compression, so equal input gives equal bytes under the same build.
pub fn write(header: &Header, rows: &[Row]) -> Result<Vec<u8>, DataError> {
    check_header(header)?;
    let mut text = format!("{MAGIC} {FORMAT} {} {}\n", header.name, header.version);
    let mut previous: Option<&str> = None;
    for (index, row) in rows.iter().enumerate() {
        let line = index + 2;
        let fail = |rule| DataError::Line { line, rule };
        if !is_valid_domain(&row.domain) {
            return Err(fail("domain"));
        }
        if row.topics.is_empty() || row.topics.known() != row.topics {
            return Err(fail("topics"));
        }
        if !is_valid_source(&row.source) {
            return Err(fail("source"));
        }
        check_order(previous, &row.domain).map_err(fail)?;
        previous = Some(&row.domain);
        text.push_str(&row.domain);
        text.push('\t');
        for (n, topic) in row.topics.iter().enumerate() {
            if n > 0 {
                text.push(',');
            }
            text.push_str(topic.name());
        }
        text.push('\t');
        text.push_str(&row.source);
        text.push('\n');
    }
    let mut encoder = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::best());
    encoder
        .write_all(text.as_bytes())
        .map_err(|_| DataError::Gzip)?;
    encoder.finish().map_err(|_| DataError::Gzip)
}

fn decompress(gz: &[u8], limits: Limits) -> Result<Vec<u8>, DataError> {
    if gz.is_empty() {
        return Err(DataError::Gzip);
    }
    let mut out = Vec::new();
    GzDecoder::new(gz)
        .take(limits.max_bytes.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|_| DataError::Gzip)?;
    if out.len() as u64 > limits.max_bytes {
        return Err(DataError::TooLarge);
    }
    Ok(out)
}

fn scan(
    text: &[u8],
    limits: Limits,
    mut each: impl FnMut(&str, TopicSet, &str),
) -> Result<(Header, usize), DataError> {
    let Some(end) = text.iter().position(|b| *b == b'\n') else {
        return Err(DataError::Header(if text.is_empty() {
            "missing"
        } else {
            "newline"
        }));
    };
    let header = parse_header(&text[..end])?;
    let mut rest = &text[end + 1..];
    let mut line = 1usize;
    let mut rows = 0usize;
    let mut skipped = 0usize;
    let mut previous: Option<&str> = None;
    while !rest.is_empty() {
        line += 1;
        rows += 1;
        if rows > limits.max_rows {
            return Err(DataError::TooManyRows);
        }
        let fail = |rule| DataError::Line { line, rule };
        let end = rest
            .iter()
            .position(|b| *b == b'\n')
            .ok_or(fail("newline"))?;
        let raw = std::str::from_utf8(&rest[..end]).map_err(|_| fail("utf8"))?;
        rest = &rest[end + 1..];
        if raw.is_empty() {
            return Err(fail("empty"));
        }
        let mut columns = raw.split('\t');
        let (Some(domain), Some(names), Some(source), None) = (
            columns.next(),
            columns.next(),
            columns.next(),
            columns.next(),
        ) else {
            return Err(fail("fields"));
        };
        if !is_valid_domain(domain) {
            return Err(fail("domain"));
        }
        let topics = parse_topics(names).ok_or(fail("topics"))?;
        if !is_valid_source(source) {
            return Err(fail("source"));
        }
        check_order(previous, domain).map_err(fail)?;
        previous = Some(domain);
        if topics.is_empty() {
            skipped += 1;
        } else {
            each(domain, topics, source);
        }
    }
    Ok((header, skipped))
}

fn parse_header(raw: &[u8]) -> Result<Header, DataError> {
    let raw = std::str::from_utf8(raw).map_err(|_| DataError::Header("utf8"))?;
    let mut parts = raw.split(' ');
    if parts.next() != Some(MAGIC) {
        return Err(DataError::Header("magic"));
    }
    let (Some(format), Some(name), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(DataError::Header("fields"));
    };
    if format != FORMAT.to_string() {
        return Err(DataError::Header("format"));
    }
    let header = Header::new(name, version);
    check_header(&header)?;
    Ok(header)
}

fn check_header(header: &Header) -> Result<(), DataError> {
    if !is_valid_name(&header.name) {
        return Err(DataError::Header("name"));
    }
    if !is_valid_version(&header.version) {
        return Err(DataError::Header("version"));
    }
    Ok(())
}

fn parse_topics(names: &str) -> Option<TopicSet> {
    let mut topics = TopicSet::EMPTY;
    for name in names.split(',') {
        if name.is_empty()
            || name.len() > 32
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return None;
        }
        if let Some(topic) = Topic::from_name(name) {
            topics.insert(topic);
        }
    }
    Some(topics)
}

fn check_order(previous: Option<&str>, domain: &str) -> Result<(), &'static str> {
    match previous.map(|p| p.as_bytes().cmp(domain.as_bytes())) {
        Some(std::cmp::Ordering::Equal) => Err("duplicate"),
        Some(std::cmp::Ordering::Greater) => Err("order"),
        _ => Ok(()),
    }
}
