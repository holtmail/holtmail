//! Parsing a Server-Sent Events stream (the WHATWG `text/event-stream` format), as JMAP push
//! (RFC 8620 §7.3) sends it. No I/O: bytes go in as they arrive, events come out.

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The `event` field, `message` when the server sent none.
    pub event: String,
    /// The `data` lines joined with `\n`.
    pub data: String,
}

/// An event or a line was longer than the parser's limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TooLarge;

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an event in the stream is too large")
    }
}

impl std::error::Error for TooLarge {}

/// Turns stream bytes into events. Lines may end in CRLF, LF or CR, and may be split across
/// chunks anywhere, also inside a UTF-8 sequence. Comments and unknown fields are skipped; an
/// event without data is not dispatched.
#[derive(Debug)]
pub struct Parser {
    limit: usize,
    line: Vec<u8>,
    after_cr: bool,
    event: String,
    data: String,
    has_data: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new(1024 * 1024)
    }
}

impl Parser {
    /// A parser that refuses lines and events longer than `limit` bytes.
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            line: Vec::new(),
            after_cr: false,
            event: String::new(),
            data: String::new(),
            has_data: false,
        }
    }

    /// Feeds the next bytes of the stream and returns the events they complete, in order.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Event>, TooLarge> {
        let mut out = Vec::new();
        for &byte in chunk {
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\r' => {
                    self.after_cr = true;
                    self.end_line(&mut out)?;
                }
                b'\n' => self.end_line(&mut out)?,
                _ => {
                    if self.line.len() >= self.limit {
                        return Err(TooLarge);
                    }
                    self.line.push(byte);
                }
            }
        }
        Ok(out)
    }

    fn end_line(&mut self, out: &mut Vec<Event>) -> Result<(), TooLarge> {
        let line = String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
        if line.is_empty() {
            if self.has_data {
                out.push(Event {
                    event: if self.event.is_empty() {
                        "message".into()
                    } else {
                        std::mem::take(&mut self.event)
                    },
                    data: std::mem::take(&mut self.data),
                });
            }
            self.event.clear();
            self.data.clear();
            self.has_data = false;
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_str(), ""),
        };
        match field {
            "event" => self.event = value.to_owned(),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
                if self.data.len() > self.limit {
                    return Err(TooLarge);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(chunks: &[&[u8]]) -> Vec<Event> {
        let mut parser = Parser::default();
        chunks
            .iter()
            .flat_map(|c| parser.push(c).unwrap())
            .collect()
    }

    #[test]
    fn parses_named_and_default_events_with_any_line_end() {
        let got = events(&[
            b"event: state\r\ndata: {\"a\":1}\r\n\r\n: keep-alive\n\ndata: x\rdata: y\r\r",
        ]);
        assert_eq!(
            got,
            vec![
                Event {
                    event: "state".into(),
                    data: "{\"a\":1}".into()
                },
                Event {
                    event: "message".into(),
                    data: "x\ny".into()
                },
            ]
        );
    }

    #[test]
    fn joins_lines_split_across_chunks_and_inside_utf8() {
        let whole = "event: ping\ndata: {\"name\":\"Zoë\"}\n\n".as_bytes();
        let split = whole.iter().position(|&b| b == 0xc3).unwrap() + 1;
        let got = events(&[&whole[..3], &whole[3..split], &whole[split..]]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].event, "ping");
        assert_eq!(got[0].data, "{\"name\":\"Zoë\"}");
    }

    #[test]
    fn a_crlf_split_between_chunks_is_one_line_end() {
        let got = events(&[b"data: a\r", b"\n\r", b"\n"]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, "a");
    }

    #[test]
    fn skips_events_without_data_and_unknown_fields() {
        assert!(events(&[b"event: ping\nid: 7\nretry: 10\n\n"]).is_empty());
        let got = events(&[b"data\n\n"]);
        assert_eq!(got[0].data, "");
    }

    #[test]
    fn refuses_oversized_lines() {
        let mut parser = Parser::new(8);
        assert_eq!(parser.push(b"data: 0123456789"), Err(TooLarge));
    }
}
