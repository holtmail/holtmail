use holtmail_store::Message;

use crate::time::civil;

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// One path segment that every common file system accepts and that cannot climb out of its
/// directory: no separators, no control characters, no `.` or `..`, at most `max` characters.
pub(crate) fn segment(raw: &str, max: usize) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(max)
        .collect();
    let trimmed = cleaned
        .trim()
        .trim_end_matches(['.', ' '])
        .trim_start_matches('.');
    if trimmed.is_empty() {
        return "_".into();
    }
    let stem = trimmed.split('.').next().unwrap_or_default();
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        return format!("_{trimmed}");
    }
    trimmed.to_owned()
}

/// A folder path with `/` between levels as a relative directory of safe segments.
pub(crate) fn directory(path: &str) -> String {
    path.split('/')
        .filter(|s| !s.trim().is_empty())
        .map(|s| segment(s, 80))
        .collect::<Vec<_>>()
        .join("/")
}

/// `2025-03-14 0900 Subject 1a2b3c4d5e6f7a8b.eml`: date, subject, and the start of the blob id,
/// which keeps names unique inside an archive and the same in every export.
pub(crate) fn entry(directory: Option<&str>, message: &Message) -> String {
    let date = civil(message.date);
    let subject = message
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("No subject");
    let hash = message.blob_id.to_string();
    let name = format!(
        "{:04}-{:02}-{:02} {:02}{:02} {} {}.eml",
        date.year,
        date.month,
        date.day,
        date.hour,
        date.minute,
        segment(subject, 60),
        &hash[..16]
    );
    match directory {
        Some(dir) if !dir.is_empty() => format!("{dir}/{name}"),
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_safe_segments() {
        assert_eq!(segment("..", 80), "_");
        assert_eq!(segment(".", 80), "_");
        assert_eq!(segment("../../etc", 80), "_.._etc");
        assert_eq!(segment("a/b\\c:d", 80), "a_b_c_d");
        assert_eq!(segment(" Re: hello. ", 80), "Re_ hello");
        assert_eq!(segment(".hidden", 80), "hidden");
        assert_eq!(segment("nul.txt", 80), "_nul.txt");
        assert_eq!(segment("line\nbreak", 80), "line_break");
        assert_eq!(segment(&"x".repeat(100), 10), "xxxxxxxxxx");
    }

    #[test]
    fn makes_directories() {
        assert_eq!(directory("INBOX"), "INBOX");
        assert_eq!(directory("Projects/2025"), "Projects/2025");
        assert_eq!(directory("/../a//b/"), "_/a/b");
        assert_eq!(directory(""), "");
    }
}
