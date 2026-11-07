//! Holtmail's own image in containers, for the upgrade and `docker exec` tests (`IT_PROFILES`
//! naming `docker`): containers on the stack's network with a named volume for `/data` and the
//! port published on `127.0.0.1`.

use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The image built from this checkout by `it.sh`.
pub const DEV_IMAGE: &str = "holtmail:dev";
/// The image built from this checkout with the `insights-test-data` feature (allowed test
/// networks for one-click unsubscribe), by `it.sh`.
pub const INSIGHTS_IMAGE: &str = "holtmail:it-insights";
/// The image built from `fed635b`, before migrations 0006–0015.
pub const OLD_IMAGE: &str = "holtmail:it-old";
/// The commit [`OLD_IMAGE`] is built from.
pub const OLD_COMMIT: &str = "fed635b";
/// The stack's network; containers reach Dovecot there as `dovecot:31143`.
pub const NETWORK: &str = "holtmail-it_default";

/// Runs `docker` with `args` and `stdin`, returning its output whatever its status.
pub fn docker(args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new("docker")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("docker runs");
    let mut input = child.stdin.take().expect("stdin");
    input.write_all(stdin).ok();
    drop(input);
    child.wait_with_output().expect("docker exits")
}

/// Runs `docker` with `args` and returns its stdout, failing the test when it fails.
#[track_caller]
pub fn docker_ok(args: &[&str]) -> String {
    let output = docker(args, b"");
    assert!(
        output.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A named volume, removed on drop.
pub struct Volume {
    /// Its name.
    pub name: String,
}

impl Volume {
    /// Creates a fresh volume.
    pub fn new(prefix: &str) -> Self {
        let name = crate::unique(prefix);
        docker_ok(&["volume", "create", &name]);
        Self { name }
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        let _ = docker(&["volume", "rm", "-f", &self.name], b"");
    }
}

/// A running Holtmail container with `volume` at `/data`; removed on drop, its logs printed
/// first when the test is failing.
pub struct Container {
    /// The container's name.
    pub name: String,
    /// `http://127.0.0.1:<published port>`.
    pub url: String,
}

impl Container {
    /// Starts `image` with `volume` on the stack's network and waits until `/api/setup`
    /// answers. `env` adds variables such as `HOLTMAIL_SYNC_INTERVAL_MINUTES`.
    pub async fn start(image: &str, volume: &Volume, env: &[(&str, &str)]) -> Self {
        Self::start_with(image, volume, env, &[]).await
    }

    /// [`Container::start`] with more `docker run` arguments, such as another `-v`.
    pub async fn start_with(
        image: &str,
        volume: &Volume,
        env: &[(&str, &str)],
        extra: &[&str],
    ) -> Self {
        Self::run(image, volume, env, extra, &[]).await
    }

    /// [`Container::start_with`] with `command` after the image instead of its default
    /// `serve`, such as `["--config", "/config/holtmail.toml", "serve"]`.
    pub async fn run(
        image: &str,
        volume: &Volume,
        env: &[(&str, &str)],
        extra: &[&str],
        command: &[&str],
    ) -> Self {
        let name = crate::unique("holtmail-it");
        let mount = format!("{}:/data", volume.name);
        let mut args = vec![
            "run",
            "-d",
            "--name",
            &name,
            "--network",
            NETWORK,
            "-p",
            "127.0.0.1::8080",
            "-v",
            &mount,
            "-e",
            "RUST_LOG=info",
            "-e",
            "NO_COLOR=1",
        ];
        let pairs: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        for pair in &pairs {
            args.extend(["-e", pair.as_str()]);
        }
        args.extend_from_slice(extra);
        args.push(image);
        args.extend_from_slice(command);
        docker_ok(&args);
        let url = published(&name);
        let container = Self { name, url };
        container.wait_ready().await;
        container
    }

    async fn wait_ready(&self) {
        let client = reqwest::Client::new();
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if let Ok(response) = client.get(format!("{}/api/setup", self.url)).send().await
                && response.status().is_success()
            {
                return;
            }
            let running = docker_ok(&["inspect", "-f", "{{.State.Running}}", &self.name]);
            assert!(
                running.trim() == "true",
                "the container exited:\n{}",
                self.logs()
            );
            assert!(
                Instant::now() < deadline,
                "the container did not answer:\n{}",
                self.logs()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Everything the container wrote so far.
    pub fn logs(&self) -> String {
        let output = docker(&["logs", &self.name], b"");
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    /// The token of the setup link the container printed.
    pub fn setup_token(&self) -> Option<String> {
        let logs = self.logs();
        let start = logs.rfind("setup?token=")? + "setup?token=".len();
        let token: String = logs[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .collect();
        (!token.is_empty()).then_some(token)
    }

    /// `docker exec -i <container> holtmail <args>` with `stdin`.
    pub fn exec(&self, args: &[&str], stdin: &str) -> Output {
        let mut full = vec!["exec", "-i", self.name.as_str(), "holtmail"];
        full.extend_from_slice(args);
        docker(&full, stdin.as_bytes())
    }

    /// Waits until the logs contain `needle`.
    pub async fn wait_log(&self, needle: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while !self.logs().contains(needle) {
            assert!(
                Instant::now() < deadline,
                "no {needle:?} in the logs:\n{}",
                self.logs()
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Stops the container (its stop signal, SIGINT) and waits for it.
    pub fn stop(&self) {
        docker_ok(&["stop", "-t", "30", &self.name]);
    }

    /// Starts the stopped container again, keeping its networks, and waits until `/api/setup`
    /// answers; [`Container::url`] follows the port Docker publishes this time.
    pub async fn start_again(&mut self) {
        docker_ok(&["start", &self.name]);
        self.url = published(&self.name);
        self.wait_ready().await;
    }

    /// Attaches the container to one more network.
    pub fn connect(&self, network: &str) {
        docker_ok(&["network", "connect", network, &self.name]);
    }

    /// Copies `path` out of the container (running or stopped) to `to` on this machine.
    pub fn copy_out(&self, path: &str, to: &std::path::Path) {
        docker_ok(&[
            "cp",
            &format!("{}:{path}", self.name),
            to.to_str().expect("UTF-8"),
        ]);
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("--- {} logs ---\n{}", self.name, self.logs());
        }
        let _ = docker(&["rm", "-f", "-v", &self.name], b"");
    }
}

fn published(name: &str) -> String {
    let port = docker_ok(&["port", name, "8080/tcp"]);
    let port = port
        .lines()
        .next()
        .and_then(|l| l.rsplit(':').next())
        .expect("a published port")
        .trim()
        .to_owned();
    format!("http://127.0.0.1:{port}")
}

/// Whether the image `name` is here.
pub fn image_present(name: &str) -> bool {
    docker(&["image", "inspect", name], b"").status.success()
}
