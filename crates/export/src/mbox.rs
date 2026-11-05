use holtmail_core::{Address, Timestamp};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::time::civil;

/// The envelope sender of the separator line: the first `From` address without spaces, or
/// `MAILER-DAEMON`.
pub(crate) fn envelope_sender(from: &[Address]) -> String {
    let sender: String = from
        .first()
        .map(|a| {
            a.email
                .chars()
                .filter(|c| !c.is_whitespace() && !c.is_control())
                .collect()
        })
        .unwrap_or_default();
    if sender.is_empty() {
        "MAILER-DAEMON".into()
    } else {
        sender
    }
}

pub(crate) fn separator(sender: &str, date: Timestamp) -> String {
    format!("From {sender} {}\n", civil(date).asctime())
}

fn needs_quote(line: &[u8]) -> bool {
    let quotes = line.iter().take_while(|&&b| b == b'>').count();
    line[quotes..].starts_with(b"From ")
}

/// Writes one message in mboxrd form: the separator line, the bytes with every line that starts
/// with `From `, `>From `, `>>From `, ... given one more `>`, and an empty line in the message's
/// own line ending.
pub(crate) async fn write_message<W: AsyncWrite + Unpin + ?Sized>(
    out: &mut W,
    raw: &[u8],
    sender: &str,
    date: Timestamp,
) -> std::io::Result<()> {
    out.write_all(separator(sender, date).as_bytes()).await?;
    let mut copied = 0;
    let mut line = 0;
    while line < raw.len() {
        if needs_quote(&raw[line..]) {
            out.write_all(&raw[copied..line]).await?;
            out.write_all(b">").await?;
            copied = line;
        }
        match raw[line..].iter().position(|&b| b == b'\n') {
            Some(end) => line += end + 1,
            None => break,
        }
    }
    out.write_all(&raw[copied..]).await?;
    let ending: &[u8] = if raw.ends_with(b"\r\n") {
        b"\r\n"
    } else if raw.ends_with(b"\n") {
        b"\n"
    } else {
        b"\n\n"
    };
    out.write_all(ending).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn written(raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_message(&mut out, raw, "ada@example.com", Timestamp::from_secs(0))
            .await
            .unwrap();
        out
    }

    #[tokio::test]
    async fn quotes_from_lines() {
        let out =
            written(b"From me\nSubject: x\n\nFrom here\n>From there\n> From not\nFromage\n").await;
        assert_eq!(
            out,
            b"From ada@example.com Thu Jan  1 00:00:00 1970\n>From me\nSubject: x\n\n>From here\n>>From there\n> From not\nFromage\n\n"
        );
    }

    #[tokio::test]
    async fn ends_with_an_empty_line_in_the_message_line_ending() {
        assert!(
            written(b"Subject: x\r\n\r\nHi\r\n")
                .await
                .ends_with(b"Hi\r\n\r\n")
        );
        assert!(written(b"Subject: x\n\nHi").await.ends_with(b"Hi\n\n"));
    }

    #[test]
    fn picks_a_sender() {
        assert_eq!(envelope_sender(&[]), "MAILER-DAEMON");
        let from = [Address {
            name: Some("Ada".into()),
            email: "ada @example.com".into(),
        }];
        assert_eq!(envelope_sender(&from), "ada@example.com");
    }
}
