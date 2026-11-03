//! The `holtmail` binary: the server and the command-line tools.

mod accounts;
mod config;
mod import;
mod mailboxes;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use holtmail_api::ApiState;
use holtmail_auth::{Auth, Signup};
use holtmail_ingest::Ingest;
use holtmail_store::{InstanceRole, Store};
use tower_http::services::{ServeDir, ServeFile};
use tracing_subscriber::EnvFilter;

use config::{Config, Services};

#[derive(Parser)]
#[command(name = "holtmail", version, about = "Self-hosted email archive")]
struct Cli {
    /// Config file; defaults apply when omitted.
    #[arg(short, long, global = true, env = "HOLTMAIL_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web interface and the API.
    Serve,
    /// Import mbox, Maildir, .eml or zip files into an archive.
    Import {
        /// Archive name; created when missing.
        #[arg(short, long)]
        archive: String,
        /// Account name inside the archive; defaults to the file name.
        #[arg(long)]
        account: Option<String>,
        /// Files or directories to import.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// List archives with their totals.
    Archives,
    /// Manage mail accounts.
    #[command(subcommand)]
    Account(AccountCommand),
    /// Manage people who sign in.
    #[command(subcommand)]
    User(UserCommand),
    /// Archive new mail from every enabled account now.
    Sync {
        /// Only this account id.
        #[arg(long)]
        account: Option<String>,
    },
    /// Rebuild the search index from the stored messages.
    Reindex,
    /// Print the API's OpenAPI description.
    Openapi,
}

#[derive(Subcommand)]
enum AccountCommand {
    /// List accounts of every archive.
    List,
    /// Add an IMAP account. The password is read from HOLTMAIL_IMAP_PASSWORD or the first line of stdin.
    AddImap {
        /// Archive name; created when missing.
        #[arg(short, long)]
        archive: String,
        /// Display name; defaults to the user name.
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        host: String,
        #[arg(long)]
        port: Option<u16>,
        /// tls, starttls or plain.
        #[arg(long, default_value = "tls")]
        security: String,
        #[arg(long)]
        username: String,
        /// Folder paths to skip, e.g. --exclude Junk --exclude Trash.
        #[arg(long)]
        exclude: Vec<String>,
    },
}

#[derive(Subcommand)]
enum UserCommand {
    /// List users.
    List,
    /// Add a user. The password is read from HOLTMAIL_PASSWORD or the first line of stdin.
    Add {
        #[arg(long)]
        email: String,
        #[arg(long)]
        name: String,
        /// owner, admin or member; the first user defaults to owner, later ones to member.
        #[arg(long)]
        role: Option<String>,
    },
    /// Set a user's password and sign them out everywhere. The password is read from
    /// HOLTMAIL_PASSWORD or the first line of stdin.
    Password {
        #[arg(long)]
        email: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Openapi => {
            println!("{}", holtmail_api::openapi_json());
            Ok(())
        }
        Command::Serve => serve(Config::load(cli.config.as_deref())?).await,
        Command::Archives => {
            let config = Config::load(cli.config.as_deref())?;
            let services = Services::open(&config).await?;
            for archive in services.store.list_archives().await? {
                let stats = services.store.archive_stats(archive.id).await?;
                println!(
                    "{}  {}  {} messages, {} attachments, {} MB",
                    archive.id,
                    archive.name,
                    stats.messages,
                    stats.attachments,
                    stats.bytes / 1_000_000
                );
            }
            Ok(())
        }
        Command::Reindex => {
            let config = Config::load(cli.config.as_deref())?;
            let services = Services::open(&config).await?;
            let ingest = Ingest::new(
                services.blobs.clone(),
                services.store.clone(),
                services.index.clone(),
            );
            let mut done = 0u64;
            for archive in services.store.list_archives().await? {
                let mut after = None;
                loop {
                    let page = services
                        .store
                        .list_messages(&holtmail_store::MessageQuery {
                            archive_id: archive.id,
                            folder_id: None,
                            after,
                            limit: 500,
                        })
                        .await?;
                    for message in &page.items {
                        if let Err(error) = ingest.reindex(message.id).await {
                            tracing::warn!(message = %message.id, %error, "not reindexed");
                        }
                    }
                    ingest.commit().await?;
                    done += page.items.len() as u64;
                    eprintln!("  {done} messages");
                    match page.next {
                        Some(next) => after = Some(next),
                        None => break,
                    }
                }
            }
            println!("Reindexed {done} messages.");
            Ok(())
        }
        Command::Account(command) => {
            let config = Config::load(cli.config.as_deref())?;
            let services = Services::open(&config).await?;
            account_command(&services, command).await
        }
        Command::User(command) => {
            let config = Config::load(cli.config.as_deref())?;
            user_command(config::open_store(&config).await?, command).await
        }
        Command::Sync { account } => {
            let config = Config::load(cli.config.as_deref())?;
            let services = Services::open(&config).await?;
            let syncer = accounts::syncer(&services);
            let mut list = accounts::syncable_accounts(&*services.store).await?;
            if let Some(id) = account {
                list.retain(|a| a.id.to_string() == id);
                anyhow::ensure!(!list.is_empty(), "no enabled account {id}");
            }
            for account in list {
                let report = accounts::sync_one(&services, &syncer, &account).await?;
                println!("{} ({})", account.name, account.id);
                for folder in report.folders {
                    println!(
                        "  {:<40} {} new, {} known, {} removed, {} failed{}",
                        folder.path,
                        folder.new,
                        folder.known,
                        folder.removed,
                        folder.failed,
                        folder.error.map(|e| format!(" — {e}")).unwrap_or_default()
                    );
                }
            }
            Ok(())
        }
        Command::Import {
            archive,
            account,
            paths,
        } => {
            let config = Config::load(cli.config.as_deref())?;
            let services = Services::open(&config).await?;
            let ingest = Ingest::new(
                services.blobs.clone(),
                services.store.clone(),
                services.index.clone(),
            );
            let archive_id = import::find_or_create_archive(&*services.store, &archive).await?;
            for path in paths {
                let account_name = account.clone().unwrap_or_else(|| {
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Import".into())
                });
                let account_id = import::find_or_create_import_account(
                    &*services.store,
                    archive_id,
                    &account_name,
                )
                .await?;
                let totals = import::import_path(
                    &*services.store,
                    &ingest,
                    archive_id,
                    account_id,
                    &path,
                    |t| {
                        eprintln!(
                            "  {} new, {} already archived, {} failed",
                            t.new, t.known, t.failed
                        )
                    },
                )
                .await?;
                println!(
                    "{}: {} new, {} already archived, {} failed",
                    path.display(),
                    totals.new,
                    totals.known,
                    totals.failed
                );
            }
            Ok(())
        }
    }
}

async fn account_command(services: &Services, command: AccountCommand) -> anyhow::Result<()> {
    match command {
        AccountCommand::List => {
            for archive in services.store.list_archives().await? {
                for account in services.store.list_accounts(archive.id).await? {
                    println!(
                        "{}  {:<8} {:<10} {}  ({})",
                        account.id,
                        account.kind.as_str(),
                        if account.enabled {
                            "enabled"
                        } else {
                            "disabled"
                        },
                        account.name,
                        archive.name
                    );
                }
            }
            Ok(())
        }
        AccountCommand::AddImap {
            archive,
            name,
            host,
            port,
            security,
            username,
            exclude,
        } => {
            let security = accounts::parse_security(&security)?;
            let archive = import::find_or_create_archive(&*services.store, &archive).await?;
            let password = read_password("HOLTMAIL_IMAP_PASSWORD", &username)?;
            let account = accounts::add_imap(
                &*services.store,
                &*services.secrets,
                accounts::NewImapAccount {
                    archive,
                    name,
                    host,
                    port,
                    security,
                    username,
                    password,
                    exclude,
                },
            )
            .await?;
            println!(
                "Added {} ({}). Run `holtmail sync` to archive it now.",
                account.name, account.id
            );
            Ok(())
        }
    }
}

fn read_password(variable: &str, whose: &str) -> anyhow::Result<String> {
    let password = match std::env::var(variable) {
        Ok(password) => password,
        Err(_) => {
            eprint!("Password for {whose}: ");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim_end_matches(['\r', '\n']).to_owned()
        }
    };
    anyhow::ensure!(!password.is_empty(), "the password is empty");
    Ok(password)
}

fn auth(store: Arc<dyn Store>) -> anyhow::Result<Arc<Auth>> {
    Ok(Arc::new(Auth::new(
        store,
        holtmail_auth::Options::default(),
    )?))
}

async fn user_command(store: Arc<dyn Store>, command: UserCommand) -> anyhow::Result<()> {
    let auth = auth(store.clone())?;
    match command {
        UserCommand::List => {
            for user in store.list_users().await? {
                println!(
                    "{}  {:<6}  {}  ({})",
                    user.id,
                    user.role.as_str(),
                    user.email,
                    user.name
                );
            }
            Ok(())
        }
        UserCommand::Add { email, name, role } => {
            let role = match role.as_deref() {
                Some(role) => InstanceRole::parse(role).with_context(|| {
                    format!("unknown role {role:?}; use owner, admin or member")
                })?,
                None if auth.setup_needed().await? => InstanceRole::Owner,
                None => InstanceRole::Member,
            };
            let password = read_password("HOLTMAIL_PASSWORD", &email)?;
            let user = auth
                .create_user(
                    Signup {
                        email,
                        name,
                        password,
                    },
                    role,
                )
                .await?;
            println!("Added {} ({}) as {}.", user.name, user.id, role.as_str());
            Ok(())
        }
        UserCommand::Password { email } => {
            let user = store
                .find_user_by_email(&email)
                .await?
                .with_context(|| format!("no user {email}"))?;
            let password = read_password("HOLTMAIL_PASSWORD", &email)?;
            auth.set_password(user.id, &password).await?;
            println!("Password changed for {}; their sessions ended.", user.name);
            Ok(())
        }
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let services = Arc::new(Services::open(&config).await?);
    let auth = auth(services.store.clone())?;
    let runner = mailboxes::Runner::new(services.clone(), config.data_dir.join("uploads"));
    let mut app = holtmail_api::router(ApiState {
        blobs: services.blobs.clone(),
        store: services.store.clone(),
        index: services.index.clone(),
        auth: auth.clone(),
        mailboxes: Arc::new(runner.clone()),
        secure_cookies: config.https(),
    });
    if config.sync_interval_minutes > 0 {
        tokio::spawn(runner.run_schedule(std::time::Duration::from_secs(
            config.sync_interval_minutes * 60,
        )));
    }
    if let Some(web) = &config.web_dir {
        let index = web.join("index.html");
        app = app.fallback_service(ServeDir::new(web).fallback(ServeFile::new(index)));
    }
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("listening on {}", config.listen))?;
    tracing::info!("Holtmail on http://{}", config.listen);
    if let Some(token) = auth.setup_token().await? {
        tracing::info!(
            "No users yet. Open {}/setup?token={token} to create the owner; the link works once.",
            config.base_url()
        );
    }
    tokio::spawn(prune_sessions(auth));
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}

async fn prune_sessions(auth: Arc<Auth>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60 * 60));
    loop {
        tick.tick().await;
        match auth.prune().await {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "expired sessions removed"),
            Err(error) => tracing::warn!(%error, "expired sessions not removed"),
        }
    }
}
