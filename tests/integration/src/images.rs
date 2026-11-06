//! The stack's image server and outbound proxies, for remote image tests.
//!
//! nginx serves `.state/images` on an internal network at `11.88.77.10`, also named
//! `images.it.test` there. `11.0.0.0/8` is public address space that is not announced on the
//! internet, so Holtmail's fetcher treats it as public, and nothing outside the stack answers
//! it. Holtmail reaches the server only through the proxies, which sit on both networks at fixed
//! addresses; nginx's access log shows which one asked.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The image server's address on the internal network.
pub const IMAGES_IP: &str = "11.88.77.10";
/// Its name, resolved only inside the stack (by the proxies with `socks5h`).
pub const IMAGES_NAME: &str = "images.it.test";
/// squid, an HTTP proxy without sign-in.
pub const SQUID_PORT: u16 = 48128;
/// A SOCKS5 proxy without sign-in.
pub const SOCKS_PORT: u16 = 48180;
/// A SOCKS5 proxy that needs [`PROXY_USER`] and [`PROXY_PASSWORD`].
pub const SOCKS_AUTH_PORT: u16 = 48181;
/// The SOCKS proxy's user.
pub const PROXY_USER: &str = "it-proxy-user";
/// The SOCKS proxy's password.
pub const PROXY_PASSWORD: &str = "it-proxy-password";
/// Where requests through squid come from, as the image server sees them.
pub const SQUID_IP: &str = "11.88.77.20";
/// Where requests through the SOCKS proxy without sign-in come from.
pub const SOCKS_IP: &str = "11.88.77.21";
/// Where requests through the SOCKS proxy with sign-in come from.
pub const SOCKS_AUTH_IP: &str = "11.88.77.22";

/// How nginx serves a file, by the first directory of its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    /// At full speed with the type of its extension.
    Plain,
    /// At 2 KiB/s, so a large file outlasts Holtmail's 15-second limit.
    Slow,
    /// At 16 KiB/s, so a run over many files takes seconds.
    Paced,
    /// As `text/html`, whatever its extension.
    Wrong,
}

impl Served {
    fn dir(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Slow => "slow",
            Self::Paced => "paced",
            Self::Wrong => "wrong",
        }
    }
}

/// The files one test puts on the image server, under a directory no other test uses.
pub struct Images {
    run: String,
}

impl Images {
    /// A fresh set.
    pub fn new(prefix: &str) -> Self {
        Self {
            run: crate::unique(prefix),
        }
    }

    /// The path a file `name` is served at.
    pub fn path(&self, served: Served, name: &str) -> String {
        format!("/{}/{}/{name}", served.dir(), self.run)
    }

    fn file(&self, path: &str) -> PathBuf {
        crate::state_dir()
            .join("images")
            .join(path.trim_start_matches('/'))
    }

    /// Writes `bytes` as `name` and returns its path.
    pub fn put(&self, served: Served, name: &str, bytes: &[u8]) -> String {
        let path = self.path(served, name);
        let file = self.file(&path);
        std::fs::create_dir_all(file.parent().expect("a directory")).expect("directory");
        std::fs::write(&file, bytes).expect("image written");
        path
    }

    /// `http://images.it.test<path>`.
    pub fn by_name(path: &str) -> String {
        format!("http://{IMAGES_NAME}{path}")
    }

    /// `http://11.88.77.10<path>`.
    pub fn by_address(path: &str) -> String {
        format!("http://{IMAGES_IP}{path}")
    }

    /// Waits until the image server serves `path` (a file written on this machine reaches the
    /// container after a moment), asking through squid.
    pub async fn wait_served(&self, path: &str) {
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{SQUID_PORT}")).expect("proxy"))
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(response) = client.head(Self::by_address(path)).send().await
                && response.status().is_success()
            {
                return;
            }
            assert!(Instant::now() < deadline, "{path} is not served");
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

/// The client address of every request the image server logged for `path`, oldest first.
pub fn requests(path: &str) -> Vec<String> {
    let logs = crate::services::compose(&["logs", "--no-log-prefix", "images"]);
    let needle = format!("\"GET {path} ");
    logs.lines()
        .filter(|line| line.contains(&needle))
        .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
        .collect()
}

/// Waits until the image server logged `count` requests for `path` and returns their client
/// addresses; the log reaches `docker compose logs` after a moment.
pub async fn wait_requests(path: &str, count: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let seen = requests(path);
        if seen.len() >= count || Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// A PNG of 24×24 pixels whose colors come from `seed`, so different seeds give different
/// bytes.
pub fn png(seed: u32) -> Vec<u8> {
    encode(seed, image::ImageFormat::Png)
}

/// A JPEG of 24×24 pixels whose colors come from `seed`.
pub fn jpeg(seed: u32) -> Vec<u8> {
    encode(seed, image::ImageFormat::Jpeg)
}

/// A PNG of about `bytes` bytes: noise from `seed` that does not compress.
pub fn noise_png(bytes: usize, seed: u32) -> Vec<u8> {
    let side = ((bytes / 3) as f64).sqrt().ceil() as u32;
    let mut state = seed | 1;
    let picture = image::RgbImage::from_fn(side, side, |_, _| {
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state & 0xff) as u8
        };
        image::Rgb([next(), next(), next()])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    picture
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("PNG encodes");
    out.into_inner()
}

/// `bytes` bytes that start like a PNG and are zeros after, for a size limit that is checked
/// before reading.
pub fn oversized(bytes: usize) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    out.resize(bytes, 0);
    out
}

/// A seed from a test's unique token.
pub fn seed(token: &str) -> u32 {
    token.bytes().fold(2_166_136_261_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(16_777_619)
    })
}

fn encode(seed: u32, format: image::ImageFormat) -> Vec<u8> {
    let [a, b, c, d] = seed.to_le_bytes();
    let picture = image::RgbImage::from_fn(24, 24, |x, y| {
        image::Rgb([
            a.wrapping_add(x as u8 * 9),
            b.wrapping_add(y as u8 * 7),
            c ^ d.wrapping_add((x * y) as u8),
        ])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut out, format).expect("image encodes");
    out.into_inner()
}

/// An HTML body showing `urls` as images.
pub fn html(urls: &[String]) -> String {
    let mut out = String::from("<html><body><p>Invented newsletter.</p>\n");
    for url in urls {
        out.push_str(&format!("<img src=\"{url}\" alt=\"\">\n"));
    }
    out.push_str("</body></html>\n");
    out
}
