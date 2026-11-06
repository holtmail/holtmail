//! Builds and runs the `holtmail` binary in a temporary data directory.

use std::io::{BufRead, BufReader, Read};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// The `holtmail` binary: `HOLTMAIL_BIN` when set, else built once per test process with
/// `cargo build -p holtmail-server` and taken from the target directory.
pub fn binary() -> PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            if let Some(path) = std::env::var_os("HOLTMAIL_BIN") {
                return PathBuf::from(path);
            }
            let root = crate::workspace_root();
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let status = Command::new(cargo)
                .args(["build", "-p", "holtmail-server", "--bin", "holtmail"])
                .current_dir(&root)
                .status()
                .expect("cargo runs");
            assert!(status.success(), "building holtmail failed");
            let target = std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("target"));
            target.join("debug").join("holtmail")
        })
        .clone()
}

/// How to configure one instance.
#[derive(Debug, Clone)]
pub struct Options {
    /// Trust the stack's CA through `tls_ca_file`.
    pub trust_stack_ca: bool,
    /// `sync_interval_minutes`; `Some(0)` syncs only on request.
    pub sync_interval_minutes: Option<u64>,
    /// Watch accounts for changes (`[watch] enabled`). Off by default, so a suite about syncs
    /// sees only the runs it asks for; on with `IT_WATCH=1` for every suite.
    pub watch: bool,
    /// More TOML appended to the config file.
    pub extra_config: String,
    /// `RUST_LOG` of the process.
    pub log_filter: String,
    /// More environment variables for the process.
    pub env: Vec<(String, String)>,
    /// Listen on this port of `127.0.0.1` instead of a free one, for a server in the stack that
    /// forwards to a fixed address.
    pub port: Option<u16>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            trust_stack_ca: true,
            sync_interval_minutes: Some(0),
            watch: std::env::var_os("IT_WATCH").is_some_and(|v| v == "1"),
            extra_config: String::new(),
            log_filter: std::env::var("IT_RUST_LOG").unwrap_or_else(|_| "info".into()),
            env: Vec::new(),
            port: None,
        }
    }
}

/// A running `holtmail serve` with its own data directory and port. Killed on drop; when the
/// test is failing, its logs are printed first.
pub struct Holtmail {
    dir: TempDir,
    port: u16,
    options: Options,
    child: Option<Child>,
    logs: Arc<Mutex<String>>,
}

impl Holtmail {
    /// Writes a config and starts `serve`, waiting until `/api/setup` answers.
    pub async fn start(options: Options) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("holtmail-it-")
            .tempdir()
            .expect("temp dir");
        let port = options.port.unwrap_or_else(free_port);
        let mut holtmail = Self {
            dir,
            port,
            options,
            child: None,
            logs: Arc::default(),
        };
        holtmail.write_config();
        holtmail.spawn().await;
        holtmail
    }

    /// Starts with the default [`Options`].
    pub async fn start_default() -> Self {
        Self::start(Options::default()).await
    }

    /// The data directory.
    pub fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    /// The config file every command of this instance uses.
    pub fn config_path(&self) -> PathBuf {
        self.dir.path().join("holtmail.toml")
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Everything the process wrote to stdout and stderr so far, across restarts.
    pub fn logs(&self) -> String {
        self.logs.lock().expect("logs lock").clone()
    }

    /// The token of the one-time setup link from the latest start, when it printed one.
    pub fn setup_token(&self) -> Option<String> {
        let logs = self.logs();
        let start = logs.rfind("setup?token=")? + "setup?token=".len();
        let token: String = logs[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .collect();
        (!token.is_empty()).then_some(token)
    }

    /// Kills the process at once (SIGKILL), as a crash would.
    pub fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Stops the process with SIGTERM and waits up to 30 seconds for it to exit.
    pub async fn stop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status();
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    /// The process id of the running `serve`.
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// The SQLite database of this instance.
    pub fn database(&self) -> PathBuf {
        self.data_dir().join("holtmail.db")
    }

    /// Moves the time every folder's flags were last read back by `hours`, as if that much time
    /// had passed since, so the next sync of a server without CONDSTORE reads them all again.
    pub async fn age_flag_checks(&self, hours: i64) {
        use sqlx::ConnectOptions;
        let mut db = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(self.database())
            .busy_timeout(Duration::from_secs(10))
            .connect()
            .await
            .expect("the database opens");
        sqlx::query(
            "UPDATE folder_cursors SET flags_checked_at = flags_checked_at - ? WHERE flags_checked_at IS NOT NULL",
        )
        .bind(hours * 3_600_000)
        .execute(&mut db)
        .await
        .expect("the cursors are updated");
    }

    /// Replaces the stored sync token of every address book and calendar with `token` and forgets
    /// their collection tags, as if the last sync had ended with a state the server no longer
    /// knows.
    pub async fn rewrite_collection_tokens(&self, token: &str) {
        use sqlx::ConnectOptions;
        let mut db = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(self.database())
            .busy_timeout(Duration::from_secs(10))
            .connect()
            .await
            .expect("the database opens");
        let done = sqlx::query(
            "UPDATE collections SET sync_token = ?, ctag = NULL WHERE sync_token IS NOT NULL",
        )
        .bind(token)
        .execute(&mut db)
        .await
        .expect("the collections are updated");
        assert!(done.rows_affected() > 0, "no collection has a sync token");
    }

    /// The ids of the items stored in the collection `collection`, of every kind, including
    /// those the API lists under neither contacts nor events.
    pub async fn item_ids(&self, collection: &str) -> Vec<String> {
        use sqlx::ConnectOptions;
        let mut db = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(self.database())
            .read_only(true)
            .busy_timeout(Duration::from_secs(10))
            .connect()
            .await
            .expect("the database opens");
        sqlx::query_scalar("SELECT id FROM items WHERE collection_id = ? ORDER BY id")
            .bind(collection)
            .fetch_all(&mut db)
            .await
            .expect("the items are read")
    }

    /// The rows `sql` selects from the database, each column read as text (`NULL` as an empty
    /// string), with `binds` bound in order.
    pub async fn query(&self, sql: &'static str, binds: &[&str]) -> Vec<Vec<String>> {
        use sqlx::{ConnectOptions, Row};
        let mut db = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(self.database())
            .read_only(true)
            .busy_timeout(Duration::from_secs(10))
            .connect()
            .await
            .expect("the database opens");
        let mut query = sqlx::query(sql);
        for bind in binds {
            query = query.bind(*bind);
        }
        query
            .fetch_all(&mut db)
            .await
            .expect("the query runs")
            .iter()
            .map(|row| {
                (0..row.len())
                    .map(|i| {
                        row.try_get::<Option<String>, _>(i)
                            .ok()
                            .flatten()
                            .or_else(|| {
                                row.try_get::<Option<i64>, _>(i)
                                    .ok()
                                    .flatten()
                                    .map(|n| n.to_string())
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .collect()
    }

    /// Runs `sql` with `binds` against the database while the instance may run, and returns how
    /// many rows it changed.
    pub async fn execute(&self, sql: &'static str, binds: &[&str]) -> u64 {
        use sqlx::ConnectOptions;
        let mut db = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(self.database())
            .busy_timeout(Duration::from_secs(10))
            .connect()
            .await
            .expect("the database opens");
        let mut query = sqlx::query(sql);
        for bind in binds {
            query = query.bind(*bind);
        }
        query
            .execute(&mut db)
            .await
            .expect("the statement runs")
            .rows_affected()
    }

    /// The secret `account` keeps sealed (a password or a refresh token), opened with this
    /// instance's `secret.key`, as the server would.
    pub async fn open_secret(&self, account: &str) -> String {
        use holtmail_secret::SecretBox;
        use sqlx::ConnectOptions;
        let mut db = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(self.database())
            .read_only(true)
            .busy_timeout(Duration::from_secs(10))
            .connect()
            .await
            .expect("the database opens");
        let sealed: Vec<u8> = sqlx::query_scalar("SELECT sealed_secret FROM accounts WHERE id = ?")
            .bind(account)
            .fetch_one(&mut db)
            .await
            .expect("the account has a sealed secret");
        let secrets = holtmail_secret_local::LocalSecretBox::open_or_create(
            self.data_dir().join("secret.key"),
        )
        .expect("the secret key");
        String::from_utf8(secrets.open(account, &sealed).expect("the secret opens"))
            .expect("a UTF-8 secret")
    }

    /// Starts `serve` again on the same data directory and port after [`Holtmail::kill`] or
    /// [`Holtmail::stop`]; does nothing while it runs.
    pub async fn start_again(&mut self) {
        if self.child.is_none() {
            self.spawn().await;
        }
    }

    /// Stops gracefully and starts again on the same data directory and port.
    pub async fn restart(&mut self) {
        self.stop().await;
        self.spawn().await;
    }

    /// Runs another `holtmail` command against this instance's config, with `stdin` as input.
    /// Its log lines (on stdout, starting with a timestamp) and stderr join [`Holtmail::logs`];
    /// what it prints as its answer stays in the returned output only.
    pub fn command(&self, args: &[&str], stdin: &str) -> Output {
        let mut child = self
            .base_command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("holtmail runs");
        {
            use std::io::Write;
            let mut input = child.stdin.take().expect("stdin");
            let _ = input.write_all(stdin.as_bytes());
        }
        let output = child.wait_with_output().expect("holtmail exits");
        let mut logs = self.logs.lock().expect("logs lock");
        for line in String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| is_log_line(line))
        {
            logs.push_str(line);
            logs.push('\n');
        }
        logs.push_str(&String::from_utf8_lossy(&output.stderr));
        output
    }

    fn write_config(&self) {
        let mut config = format!(
            "listen = \"127.0.0.1:{}\"\ndata_dir = {:?}\n",
            self.port,
            path_str(&self.data_dir())
        );
        if self.options.trust_stack_ca {
            config.push_str(&format!(
                "tls_ca_file = {:?}\n",
                path_str(&crate::ca_file())
            ));
        }
        if let Some(minutes) = self.options.sync_interval_minutes {
            config.push_str(&format!("sync_interval_minutes = {minutes}\n"));
        }
        config.push_str(&self.options.extra_config);
        std::fs::write(self.config_path(), config).expect("config written");
    }

    /// A `holtmail` command on this instance's config and environment, for a caller that needs
    /// the process itself (to sample its memory, say); unlike [`Holtmail::command`] nothing it
    /// writes joins [`Holtmail::logs`].
    pub fn cli(&self) -> Command {
        self.base_command()
    }

    fn base_command(&self) -> Command {
        let mut command = Command::new(binary());
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("HOLTMAIL_") {
                command.env_remove(key);
            }
        }
        command
            .arg("--config")
            .arg(self.config_path())
            .env("RUST_LOG", &self.options.log_filter)
            .env("NO_COLOR", "1")
            .env(
                "HOLTMAIL_WATCH",
                if self.options.watch { "true" } else { "false" },
            )
            .envs(self.options.env.iter().map(|(k, v)| (k, v)));
        command
    }

    async fn spawn(&mut self) {
        let mut child = self
            .base_command()
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("holtmail serve starts");
        capture(child.stdout.take().expect("stdout"), self.logs.clone());
        capture(child.stderr.take().expect("stderr"), self.logs.clone());
        self.child = Some(child);
        self.wait_ready().await;
    }

    async fn wait_ready(&mut self) {
        let client = reqwest::Client::new();
        let url = format!("{}/api/setup", self.url());
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok(response) = client.get(&url).send().await
                && response.status().is_success()
            {
                return;
            }
            if let Some(child) = self.child.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                panic!("holtmail exited with {status}:\n{}", self.logs());
            }
            assert!(
                Instant::now() < deadline,
                "holtmail did not answer within 60 seconds:\n{}",
                self.logs()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Drop for Holtmail {
    fn drop(&mut self) {
        self.kill();
        if std::thread::panicking() {
            eprintln!(
                "--- holtmail logs ({}) ---\n{}",
                self.dir.path().display(),
                self.logs()
            );
        }
    }
}

fn capture(stream: impl Read + Send + 'static, logs: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            let mut logs = logs.lock().expect("logs lock");
            logs.push_str(&line);
            logs.push('\n');
        }
    });
}

fn is_log_line(line: &str) -> bool {
    line.split_whitespace()
        .next()
        .is_some_and(|first| chrono::DateTime::parse_from_rfc3339(first).is_ok())
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port()
}

fn path_str(path: &Path) -> String {
    path.to_str().expect("UTF-8 path").to_owned()
}
