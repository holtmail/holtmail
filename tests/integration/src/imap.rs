//! A small IMAP client for seeding servers: sign in, create folders, append messages and run
//! any other command. Plain TCP only; seeding is not what the tests check.

use chrono::{DateTime, Utc};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// A signed-in IMAP connection.
pub struct Seeder {
    stream: BufReader<TcpStream>,
    tag: u32,
}

impl Seeder {
    /// Connects to `address` (`host:port`, plain IMAP) and signs in with `LOGIN`.
    pub async fn login(address: &str, user: &str, password: &str) -> Self {
        let stream = TcpStream::connect(address)
            .await
            .unwrap_or_else(|e| panic!("IMAP at {address}: {e}"));
        let mut seeder = Self {
            stream: BufReader::new(stream),
            tag: 0,
        };
        let greeting = seeder.line().await;
        assert!(greeting.starts_with("* OK"), "greeting: {greeting}");
        seeder
            .ok(&format!("LOGIN {} {}", quote(user), quote(password)))
            .await;
        seeder
    }

    /// Creates `folder` (a name the server takes as is, ASCII or already in modified UTF-7).
    pub async fn create(&mut self, folder: &str) {
        self.ok(&format!("CREATE {}", quote(folder))).await;
    }

    /// Appends `message` to `folder` with `flags` (such as `\Seen`) and, when given, an internal
    /// date; returns the server's tagged answer.
    pub async fn append(
        &mut self,
        folder: &str,
        flags: &[&str],
        internal_date: Option<DateTime<Utc>>,
        message: &[u8],
    ) -> String {
        let mut command = format!("APPEND {} ({})", quote(folder), flags.join(" "));
        if let Some(date) = internal_date {
            command.push_str(&format!(" \"{}\"", date.format("%d-%b-%Y %H:%M:%S +0000")));
        }
        command.push_str(&format!(" {{{}}}", message.len()));
        let tag = self.send(&command).await;
        let ready = self.line().await;
        assert!(ready.starts_with('+'), "APPEND was refused: {ready}");
        self.stream
            .get_mut()
            .write_all(message)
            .await
            .expect("write");
        self.stream
            .get_mut()
            .write_all(b"\r\n")
            .await
            .expect("write");
        let (answer, _) = self.finish(&tag).await;
        assert!(answer.contains(" OK"), "APPEND failed: {answer}");
        answer
    }

    /// Appends every message of `messages` to `folder` without flags, sending up to 200 at a time
    /// before reading the answers (`LITERAL+`, which Dovecot and GreenMail offer).
    pub async fn append_many(&mut self, folder: &str, messages: &[Vec<u8>]) {
        for chunk in messages.chunks(200) {
            let mut tags = Vec::with_capacity(chunk.len());
            for message in chunk {
                self.tag += 1;
                let tag = format!("s{}", self.tag);
                let mut command = format!(
                    "{tag} APPEND {} () {{{}+}}\r\n",
                    quote(folder),
                    message.len()
                )
                .into_bytes();
                command.extend_from_slice(message);
                command.extend_from_slice(b"\r\n");
                self.stream
                    .get_mut()
                    .write_all(&command)
                    .await
                    .expect("write");
                tags.push(tag);
            }
            for tag in tags {
                let (answer, _) = self.finish(&tag).await;
                assert!(answer.contains(" OK"), "APPEND failed: {answer}");
            }
        }
    }

    /// The `STATUS` items (such as `UIDVALIDITY` or `MESSAGES`) of `folder`, as numbers.
    pub async fn status(&mut self, folder: &str, item: &str) -> u64 {
        let lines = self.ok(&format!("STATUS {} ({item})", quote(folder))).await;
        let line = lines
            .iter()
            .find(|l| l.starts_with("* STATUS"))
            .unwrap_or_else(|| panic!("no STATUS answer for {folder}"));
        let after = &line[line.find(item).expect("the item") + item.len()..];
        after
            .trim_start()
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no number in {line}"))
    }

    /// Runs `command` and returns the tagged answer with the untagged lines before it.
    pub async fn command(&mut self, command: &str) -> (String, Vec<String>) {
        let tag = self.send(command).await;
        self.finish(&tag).await
    }

    /// Runs `command` and checks that the server answered `OK`.
    pub async fn ok(&mut self, command: &str) -> Vec<String> {
        let (answer, lines) = self.command(command).await;
        let verb = command.split(' ').next().unwrap_or(command);
        assert!(answer.contains(" OK"), "{verb} failed: {answer}");
        lines
    }

    /// Signs out and closes the connection.
    pub async fn logout(mut self) {
        let _ = self.command("LOGOUT").await;
    }

    async fn send(&mut self, command: &str) -> String {
        self.tag += 1;
        let tag = format!("s{}", self.tag);
        self.stream
            .get_mut()
            .write_all(format!("{tag} {command}\r\n").as_bytes())
            .await
            .expect("write");
        tag
    }

    async fn finish(&mut self, tag: &str) -> (String, Vec<String>) {
        let mut lines = Vec::new();
        loop {
            let line = self.line().await;
            if line.starts_with(&format!("{tag} ")) {
                return (line, lines);
            }
            lines.push(line);
        }
    }

    async fn line(&mut self) -> String {
        let mut line = String::new();
        let read = self.stream.read_line(&mut line).await.expect("read");
        assert!(read > 0, "the IMAP server closed the connection");
        line.trim_end().to_owned()
    }
}

/// `name` in modified UTF-7 (RFC 3501), as IMAP folder names are sent.
pub fn utf7(name: &str) -> String {
    use base64::Engine;
    let mut out = String::new();
    let mut pending: Vec<u16> = Vec::new();
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if pending.is_empty() {
            return;
        }
        let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
        let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes);
        out.push('&');
        out.push_str(&encoded.replace('/', ","));
        out.push('-');
        pending.clear();
    };
    for c in name.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut pending, &mut out);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            let mut units = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut units));
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// An IMAP quoted string.
pub fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    #[test]
    fn encodes_folder_names_in_modified_utf7() {
        assert_eq!(
            super::utf7("Отправленные"),
            "&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-"
        );
        assert_eq!(super::utf7("A & B"), "A &- B");
        assert_eq!(
            super::utf7("Projects/Важное"),
            "Projects/&BBIEMAQ2BD0EPgQ1-"
        );
    }
}
