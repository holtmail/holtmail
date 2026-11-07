//! Plain SMTP submission to a server of the stack, such as Postfix with OpenDKIM in `sl-mx`.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

async fn reply(io: &mut BufReader<TcpStream>) -> String {
    loop {
        let mut line = String::new();
        io.read_line(&mut line).await.expect("an SMTP reply");
        if line.as_bytes().get(3) != Some(&b'-') {
            return line.trim_end().to_owned();
        }
    }
}

/// Sends `data` (CRLF line ends, no line starting with `.`) from `from` to `to` over plain SMTP
/// on `127.0.0.1:port` and returns the server's answer to the data.
pub async fn send(port: u16, from: &str, to: &str, data: &[u8]) -> String {
    let mut io = BufReader::new(
        TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("the SMTP server answers"),
    );
    reply(&mut io).await;
    for command in [
        "EHLO client.example.com\r\n".to_owned(),
        format!("MAIL FROM:<{from}>\r\n"),
        format!("RCPT TO:<{to}>\r\n"),
        "DATA\r\n".to_owned(),
    ] {
        io.get_mut()
            .write_all(command.as_bytes())
            .await
            .expect("written");
        let answer = reply(&mut io).await;
        assert!(
            answer.starts_with('2') || answer.starts_with('3'),
            "{command:?}: {answer}"
        );
    }
    io.get_mut().write_all(data).await.expect("written");
    if !data.ends_with(b"\r\n") {
        io.get_mut().write_all(b"\r\n").await.expect("written");
    }
    io.get_mut().write_all(b".\r\n").await.expect("written");
    let answer = reply(&mut io).await;
    io.get_mut().write_all(b"QUIT\r\n").await.expect("written");
    answer
}
