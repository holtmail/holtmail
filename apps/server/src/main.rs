//! The `holtmail` binary: the server and the command-line tools.

mod accounts;
mod backup;
mod config;
mod dav;
mod deletion;
mod export;
mod image_proxy;
mod import;
mod ledger;
mod mailboxes;
mod oauth;
mod operations;
mod people;
mod schedule;
mod smtp;
mod tasks;
mod webhooks;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use holtmail_api::ApiState;
use holtmail_auth::{Auth, Signup};
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
    /// Import mbox, Maildir, .eml, .vcf, .ics or zip files (Google Takeout too) into an archive.
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
    /// Rebuild the search index from the stored messages, and the listed fields of contacts and
    /// events from their latest versions.
    Reindex {
        /// Read the text of every attachment again instead of reusing what was read before,
        /// e.g. after an upgrade that reads more file types.
        #[arg(long)]
        refresh_attachments: bool,
    },
    /// Write the messages of an archive, a folder, a label or a search as mbox or as a zip of
    /// .eml files; with --contacts or --calendar, its contacts as .vcf or its calendar items as
    /// .ics (latest versions), or every version as a zip. Works while `serve` runs, except for
    /// --query.
    Export {
        /// Archive name or id.
        #[arg(short, long)]
        archive: String,
        /// Only this folder: its path, `account name/path`, or its id.
        #[arg(long, conflicts_with_all = ["contacts", "calendar"])]
        folder: Option<String>,
        /// Only the messages with this label.
        #[arg(long, conflicts_with_all = ["contacts", "calendar"])]
        label: Option<String>,
        /// Only the messages matching this search, e.g. "from:ada@example.com before:2024-01-01".
        #[arg(long, conflicts_with_all = ["contacts", "calendar"])]
        query: Option<String>,
        /// Export contacts instead of messages.
        #[arg(long, conflicts_with = "calendar")]
        contacts: bool,
        /// Export calendar items instead of messages.
        #[arg(long)]
        calendar: bool,
        /// With --contacts or --calendar: only this address book or calendar, by name or id.
        #[arg(long)]
        collection: Option<String>,
        /// With --contacts: only the cards of this person id.
        #[arg(long, requires = "contacts", conflicts_with = "collection")]
        person: Option<String>,
        /// With --calendar and --to: only events with an instance on or after this day
        /// (YYYY-MM-DD, UTC).
        #[arg(long, requires_all = ["calendar", "to"])]
        from: Option<String>,
        /// With --calendar and --from: only events with an instance before this day
        /// (YYYY-MM-DD, UTC).
        #[arg(long, requires_all = ["calendar", "from"])]
        to: Option<String>,
        /// With --contacts or --calendar: put items removed from their source into a .vcf or
        /// .ics too; a zip has them always.
        #[arg(long)]
        include_removed: bool,
        /// mbox, zip or pdf (a zip of PDFs) for messages, vcf or zip for contacts, ics or zip for calendars; taken
        /// from the output's extension when omitted (zip for .zip, else mbox, vcf or ics).
        #[arg(long)]
        format: Option<String>,
        /// The file to write, or - for standard output. An existing file is not overwritten.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Write a backup of the whole instance to one tar file: a snapshot of the database, every
    /// blob, and secret.key only when asked. Safe while `serve` runs.
    Backup {
        /// The tar file to write, or - for standard output. An existing file is not overwritten.
        #[arg(short, long)]
        output: PathBuf,
        /// Leave the blobs out, e.g. when they are in an S3 bucket that is backed up on its own.
        #[arg(long)]
        skip_blobs: bool,
        /// Put secret.key in the backup. Anyone with the backup can then open the stored mailbox
        /// passwords and sign-in tokens; keep it as safe as the key.
        #[arg(long)]
        include_secret_key: bool,
    },
    /// Restore a backup into a new, empty data directory, checking every blob against its hash,
    /// and rebuild the search index.
    Restore {
        /// The tar file written by `holtmail backup`, or - for standard input.
        #[arg(short, long)]
        input: PathBuf,
        /// The data directory to restore into; it must not exist or be empty. Blobs go to the
        /// blob store the config names (the data directory's blobs by default).
        #[arg(long)]
        data_dir: PathBuf,
        /// Do not rebuild the search index now; `serve` rebuilds it in the background at start.
        #[arg(long)]
        skip_reindex: bool,
    },
    /// Print the API's OpenAPI description.
    Openapi,
    /// Delete the messages of an archive that a search finds, after showing what goes. They are
    /// never archived again. Needs the search index, so stop `serve` first.
    Delete {
        /// Archive name or id.
        #[arg(long)]
        archive: String,
        /// The search, as in the web app, e.g. "from:news@example.com before:2020-01-01".
        #[arg(long)]
        query: String,
        /// Delete without asking to type the number of messages.
        #[arg(long)]
        yes: bool,
    },
    /// Look at the archive ledger, the hash chain of every message archived and deleted.
    #[command(subcommand)]
    Ledger(LedgerCommand),
}

#[derive(Subcommand)]
enum LedgerCommand {
    /// Print the newest entry's position and hash; write them down elsewhere to prove later
    /// that nothing up to them changed. Works while `serve` runs.
    Head,
    /// Check the chain and compare it with the messages. Exits with 1 when anything is wrong.
    /// Works while `serve` runs.
    Verify {
        /// Also read every message's bytes and compare them with their hash.
        #[arg(long)]
        blobs: bool,
        /// A position and hash written down earlier, as <position>:<hash>.
        #[arg(long)]
        anchor: Option<String>,
    },
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
    /// Add a JMAP account, such as Fastmail. Signs in first. The password is read from
    /// HOLTMAIL_JMAP_PASSWORD or the first line of stdin; with --token-stdin an API token is read
    /// from stdin instead.
    AddJmap {
        /// Archive name; created when missing.
        #[arg(short, long)]
        archive: String,
        /// Display name; defaults to the user name, or the server's host for a token.
        #[arg(long)]
        name: Option<String>,
        /// The session address, e.g. https://api.fastmail.com/jmap/session, or the server's
        /// address, where /.well-known/jmap is asked.
        #[arg(long)]
        url: String,
        /// User name for a password; not needed with --token-stdin.
        #[arg(long)]
        username: Option<String>,
        /// Sign in with an API token read from the first line of stdin.
        #[arg(long)]
        token_stdin: bool,
        /// Mailbox paths to skip, e.g. --exclude Spam --exclude Trash.
        #[arg(long)]
        exclude: Vec<String>,
    },
    /// Change an account: its name, connection, sync interval, minimum age, or pause it. A new
    /// connection or password is checked by signing in before anything is stored. Works while
    /// `serve` runs, which picks the change up at its next check.
    Edit(EditAccount),
    /// Add a Gmail or Microsoft account that signs in with OAuth. Prints the sign-in page, then
    /// reads the address the browser lands on from stdin.
    AddOauth {
        /// Archive name; created when missing.
        #[arg(short, long)]
        archive: String,
        /// google or microsoft; guessed from the address when omitted.
        #[arg(long)]
        provider: Option<String>,
        /// The address to sign in as.
        #[arg(long)]
        email: String,
        /// Display name; defaults to the address.
        #[arg(long)]
        name: Option<String>,
    },
    /// Add a Microsoft 365, Outlook.com or Hotmail mailbox read through Microsoft Graph, for
    /// tenants where IMAP is turned off. Signs in with Microsoft like add-oauth: prints the
    /// sign-in page, then reads the address the browser lands on from stdin.
    AddGraph {
        /// Archive name; created when missing.
        #[arg(short, long)]
        archive: String,
        /// The address to sign in as.
        #[arg(long)]
        email: String,
        /// Display name; defaults to the address.
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(clap::Args)]
struct EditAccount {
    /// The account id, as `holtmail account list` shows it.
    id: String,
    /// Display name.
    #[arg(long)]
    name: Option<String>,
    /// Server host name. Another host or port needs --password-stdin with the password.
    #[arg(long)]
    host: Option<String>,
    /// Server port; the default of the security when the security changes without it.
    #[arg(long)]
    port: Option<u16>,
    /// tls, starttls or plain.
    #[arg(long)]
    security: Option<String>,
    /// User name to sign in as.
    #[arg(long)]
    username: Option<String>,
    /// JMAP accounts: the session address or the server's address. Another server needs
    /// --password-stdin with the password or token.
    #[arg(long)]
    url: Option<String>,
    /// JMAP accounts: password or token. A change needs --password-stdin with the new secret.
    #[arg(long)]
    auth: Option<String>,
    /// Folder paths to skip, replacing the stored ones, e.g. --exclude Junk --exclude Trash.
    #[arg(long)]
    exclude: Vec<String>,
    /// Archive every folder again.
    #[arg(long, conflicts_with = "exclude")]
    clear_exclude: bool,
    /// Minutes between syncs, 0 for only on request, or `default` to follow the server's.
    #[arg(long)]
    interval: Option<String>,
    /// Stop syncing the account.
    #[arg(long, conflicts_with = "resume")]
    pause: bool,
    /// Sync the account again.
    #[arg(long)]
    resume: bool,
    /// Archive only mail at least this many days old; younger mail waits for a later sync. 0
    /// archives all mail.
    #[arg(long)]
    min_age_days: Option<u32>,
    /// Read a new password, or a JMAP account's new token, from the first line of standard
    /// input; checked by signing in.
    #[arg(long)]
    password_stdin: bool,
    /// Archive the account's contacts and calendars after each mail sync: on or off. Turning
    /// on signs in first: to the CardDAV and CalDAV server, to Microsoft Graph for a Graph
    /// account, or to the JMAP session of a JMAP account that offers contacts (without
    /// --dav-url). A Google or Graph account then has to sign in again (Sign in again in the web
    /// app) to grant access.
    #[arg(long, value_parser = ["on", "off"])]
    contacts_calendars: Option<String>,
    /// The CardDAV and CalDAV address, for both; detected from the account's address when
    /// omitted (Fastmail, iCloud, Yandex, else the mail domain's well-known address). For
    /// Nextcloud: https://host/remote.php/dav.
    #[arg(long, requires = "contacts_calendars")]
    dav_url: Option<String>,
    /// The CardDAV address, when it differs from the CalDAV one.
    #[arg(long, requires = "contacts_calendars")]
    carddav_url: Option<String>,
    /// The CalDAV address, when it differs from the CardDAV one.
    #[arg(long, requires = "contacts_calendars")]
    caldav_url: Option<String>,
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
        Command::Delete {
            archive,
            query,
            yes,
        } => {
            let config = Config::load(cli.config.as_deref())?;
            let services = Services::open(&config).await?;
            let archive = export::find_archive(&*services.store, &archive).await?;
            let actor = None;
            let reviews = deletion::Reviews::default();
            let review = reviews
                .review(
                    &services,
                    archive,
                    holtmail_api::DeletionSelection::Query(query),
                    actor,
                )
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let day = |t: holtmail_core::Timestamp| {
                chrono::DateTime::from_timestamp_millis(t.as_millis())
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_default()
            };
            println!(
                "{} messages, {} bytes, from {} to {}:",
                review.count,
                review.bytes,
                review.oldest.map(day).unwrap_or_default(),
                review.newest.map(day).unwrap_or_default()
            );
            for message in review.sample.iter().take(10) {
                println!(
                    "  {}  {}",
                    day(message.date),
                    message.subject.as_deref().unwrap_or("(no subject)")
                );
            }
            if !yes {
                eprint!("Type {} to delete them for good: ", review.count);
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                anyhow::ensure!(line.trim() == review.count.to_string(), "nothing deleted");
            }
            let confirmed = reviews
                .confirm(&review.id, review.count, actor)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let tasks = Arc::new(tasks::Tasks::default());
            let task = tasks.start(tasks::NewTask {
                kind: holtmail_api::TaskKind::DeleteMessages,
                archive: Some(archive),
                account: None,
                title: confirmed.title,
                cancellable: false,
                job: None,
            });
            let done = deletion::delete(
                &services,
                &services.ingest(),
                archive,
                &confirmed.ids,
                actor,
                &task,
            )
            .await?;
            people::rebuild(&*services.store, archive).await;
            println!(
                "Deleted {} messages and {} files.",
                done.messages, done.blobs
            );
            Ok(())
        }
        Command::Ledger(command) => {
            let config = Config::load(cli.config.as_deref())?;
            let store = config::open_store(&config).await?;
            match command {
                LedgerCommand::Head => match store.ledger_head().await? {
                    Some(head) => {
                        println!("{}:{}", head.seq, head.hash);
                        Ok(())
                    }
                    None => {
                        println!("The ledger is empty.");
                        Ok(())
                    }
                },
                LedgerCommand::Verify { blobs, anchor } => {
                    let anchor = anchor.as_deref().map(ledger::parse_anchor).transpose()?;
                    let entered = ledger::enter_unentered(&*store).await?;
                    if entered > 0 {
                        eprintln!("  {entered} messages entered in the ledger first");
                    }
                    let blob_store = config::open_blobs(&config).await?;
                    let cancel = std::sync::atomic::AtomicBool::new(false);
                    let verification = ledger::verify(
                        &*store,
                        &*blob_store,
                        holtmail_api::VerifyLedger { blobs, anchor },
                        &cancel,
                        |_, _| {},
                    )
                    .await?;
                    ledger::keep(&*store, &verification).await?;
                    let hooks = webhooks::Hooks::new(
                        store.clone(),
                        config::open_secrets(&config)?,
                        Arc::new(holtmail_webhook_http::HttpWebhookSender::new(
                            std::time::Duration::from_secs(10),
                        )?),
                    );
                    ledger::announce(&hooks, &verification).await;
                    for problem in &verification.problems {
                        println!("{problem}");
                    }
                    if let (Some(seq), Some(hash)) = (verification.head_seq, verification.head_hash)
                    {
                        println!("Head {seq}:{hash}");
                    }
                    if verification.ok {
                        println!("{} entries checked, nothing wrong.", verification.entries);
                        Ok(())
                    } else {
                        println!("{} problems found.", verification.problem_count);
                        std::process::exit(1);
                    }
                }
            }
        }
        Command::Export {
            archive,
            folder,
            label,
            query,
            contacts,
            calendar,
            collection,
            person,
            from,
            to,
            include_removed,
            format,
            output,
        } => {
            let config = Config::load(cli.config.as_deref())?;
            if contacts || calendar {
                return export::run_pim(
                    &config,
                    export::PimArgs {
                        archive,
                        calendar,
                        collection,
                        person,
                        from,
                        to,
                        include_removed,
                        format,
                        output,
                    },
                )
                .await;
            }
            anyhow::ensure!(
                collection.is_none() && !include_removed,
                "--collection and --include-removed go with --contacts or --calendar"
            );
            export::run(
                &config,
                export::ExportArgs {
                    archive,
                    folder,
                    label,
                    query,
                    format,
                    output,
                },
            )
            .await
        }
        Command::Backup {
            output,
            skip_blobs,
            include_secret_key,
        } => {
            let config = Config::load(cli.config.as_deref())?;
            backup::backup(
                &config,
                backup::BackupArgs {
                    output,
                    skip_blobs,
                    include_secret_key,
                },
            )
            .await
        }
        Command::Restore {
            input,
            data_dir,
            skip_reindex,
        } => {
            let mut config = Config::load(cli.config.as_deref())?;
            config.data_dir = data_dir;
            backup::restore(
                &config,
                backup::RestoreArgs {
                    input,
                    skip_reindex,
                },
            )
            .await
        }
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
        Command::Reindex {
            refresh_attachments,
        } => {
            let config = Config::load(cli.config.as_deref())?;
            let services = Services::open(&config).await?;
            if refresh_attachments {
                let orphans = services
                    .store
                    .clear_derived(holtmail_store::DerivedKind::Text)
                    .await?;
                for blob in &orphans {
                    services.blobs.delete(blob).await?;
                }
                eprintln!("  {} saved attachment texts dropped", orphans.len());
            }
            let ingest = services.ingest();
            let done = reindex(&*services.store, &ingest, Scope::All, |done| {
                eprintln!("  {done} messages")
            })
            .await?;
            println!("Reindexed {done} messages.");
            let (messages, versions) = read_invitations(&*services.store, &ingest).await?;
            println!("Read calendar parts of {messages} messages: {versions} new event versions.");
            let rebuilt = holtmail_people::rebuild_all(&*services.store).await?;
            services
                .store
                .set_setting(people::PEOPLE_SETTING, Some("1"))
                .await?;
            println!(
                "Rebuilt {} people: {} with contacts, {} from mail alone.",
                rebuilt.people, rebuilt.with_cards, rebuilt.from_mail
            );
            Ok(())
        }
        Command::Account(AccountCommand::Edit(args)) => {
            let config = Config::load(cli.config.as_deref())?;
            edit_account(&config, args).await
        }
        Command::Account(AccountCommand::List) => {
            let config = Config::load(cli.config.as_deref())?;
            list_accounts(&*config::open_store(&config).await?).await
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
                        "  {:<40} {} new, {} known, {} removed, {} changed, {} failed{}",
                        folder.path,
                        folder.new,
                        folder.known,
                        folder.removed,
                        folder.flags_changed,
                        folder.failed,
                        folder.error.map(|e| format!(" — {e}")).unwrap_or_default()
                    );
                }
                sync_contacts(&services, &account).await;
                people::rebuild(&*services.store, account.archive_id).await;
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
            let ingest = services.ingest();
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
                    None,
                    |t| eprintln!("  {}", t.summary()),
                )
                .await?;
                println!("{}: {}", path.display(), totals.summary());
            }
            let rebuilt = holtmail_people::rebuild(&*services.store, archive_id).await?;
            println!(
                "People: {} ({} with contacts, {} from mail alone).",
                rebuilt.people, rebuilt.with_cards, rebuilt.from_mail
            );
            Ok(())
        }
    }
}

async fn account_command(services: &Services, command: AccountCommand) -> anyhow::Result<()> {
    match command {
        AccountCommand::List | AccountCommand::Edit(_) => {
            unreachable!("handled without the services")
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
        AccountCommand::AddJmap {
            archive,
            name,
            url,
            username,
            token_stdin,
            exclude,
        } => {
            let auth = if token_stdin {
                holtmail_source_jmap::JmapAuth::Token
            } else {
                holtmail_source_jmap::JmapAuth::Password
            };
            let settings =
                accounts::jmap_settings(&url, username.as_deref().unwrap_or_default(), auth)?;
            let secret = if token_stdin {
                eprint!("API token: ");
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                let token = line.trim().to_owned();
                anyhow::ensure!(!token.is_empty(), "the token is empty");
                token
            } else {
                read_password("HOLTMAIL_JMAP_PASSWORD", &settings.username)?
            };
            accounts::check_jmap_sign_in(&settings, &secret)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let archive = import::find_or_create_archive(&*services.store, &archive).await?;
            let account = accounts::add_jmap(
                &*services.store,
                &*services.secrets,
                accounts::NewJmapAccount {
                    archive,
                    name,
                    settings,
                    secret,
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
        AccountCommand::AddOauth {
            archive,
            provider,
            email,
            name,
        } => {
            let provider = match provider.as_deref() {
                Some(raw) => holtmail_oauth::Provider::parse(raw)
                    .with_context(|| format!("unknown provider {raw:?}; use google or microsoft"))?,
                None => holtmail_oauth::Provider::for_address(&email).context(
                    "cannot tell the provider from the address; pass --provider google or microsoft",
                )?,
            };
            sign_in_from_cli(
                services,
                provider,
                holtmail_oauth::Access::Imap,
                &archive,
                &email,
                name,
            )
            .await
        }
        AccountCommand::AddGraph {
            archive,
            email,
            name,
        } => {
            sign_in_from_cli(
                services,
                holtmail_oauth::Provider::Microsoft,
                holtmail_oauth::Access::Graph,
                &archive,
                &email,
                name,
            )
            .await
        }
    }
}

async fn sign_in_from_cli(
    services: &Services,
    provider: holtmail_oauth::Provider,
    access: holtmail_oauth::Access,
    archive: &str,
    email: &str,
    name: Option<String>,
) -> anyhow::Result<()> {
    anyhow::ensure!(email.contains('@'), "--email is not an address");
    let archive = import::find_or_create_archive(&*services.store, archive).await?;
    let started = services.oauth.start(
        provider,
        access,
        email.trim(),
        oauth::Purpose::NewAccount { archive, name },
        "/settings".into(),
    )?;
    if started.thunderbird {
        eprintln!(
            "Using Thunderbird's public client: the consent screen names Thunderbird. Set [oauth.{}] in the config to use your own.",
            provider.as_str()
        );
    }
    eprintln!("Open this address in a browser and sign in as {email}:\n");
    println!("{}", started.url);
    eprint!(
        "\nAfter you allow access the browser shows a page that cannot be opened.\nPaste that page's address here: "
    );
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let account = accounts::finish_oauth(services, &line).await?;
    println!(
        "Added {} ({}). Run `holtmail sync` to archive it now.",
        account.name, account.id
    );
    Ok(())
}

async fn list_accounts(store: &dyn Store) -> anyhow::Result<()> {
    for archive in store.list_archives().await? {
        for account in store.list_accounts(archive.id).await? {
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

async fn edit_account(config: &Config, args: EditAccount) -> anyhow::Result<()> {
    let store = config::open_store(config).await?;
    let id: holtmail_core::AccountId = args
        .id
        .parse()
        .map_err(|_| anyhow::anyhow!("{:?} is not an account id", args.id))?;
    let account = store
        .get_account(id)
        .await?
        .with_context(|| format!("no account {id}"))?;
    let jmap_connects = args.url.is_some() || args.auth.is_some();
    let connects = args.host.is_some()
        || args.port.is_some()
        || args.security.is_some()
        || args.username.is_some()
        || !args.exclude.is_empty()
        || args.clear_exclude
        || jmap_connects;
    let exclude = |current: Vec<String>| {
        if args.clear_exclude {
            Vec::new()
        } else if args.exclude.is_empty() {
            current
        } else {
            args.exclude.clone()
        }
    };
    let jmap = match accounts::jmap_connection(&account) {
        Some(_) if args.host.is_some() || args.port.is_some() || args.security.is_some() => {
            anyhow::bail!("a JMAP account has an address (--url), not a host, port or security")
        }
        Some(current) if connects => Some(holtmail_api::JmapConnectionChange {
            url: args.url.clone().unwrap_or(current.url),
            username: args.username.clone().unwrap_or(current.username),
            auth: args.auth.clone().unwrap_or(current.auth),
            exclude: exclude(current.exclude),
        }),
        None if jmap_connects => anyhow::bail!("only JMAP accounts have --url and --auth"),
        _ => None,
    };
    let connection = match accounts::connection(&account) {
        Some(current) if connects => {
            let new_security = args.security.filter(|s| *s != current.security);
            Some(holtmail_api::ConnectionChange {
                host: args.host.unwrap_or(current.host),
                port: args.port.or(new_security.is_none().then_some(current.port)),
                security: new_security.unwrap_or(current.security),
                username: args.username.clone().unwrap_or(current.username),
                exclude: exclude(current.exclude),
            })
        }
        None if connects && jmap.is_none() => anyhow::bail!("an import account has no server"),
        _ => None,
    };
    let sync_interval_minutes = match args.interval.as_deref().map(str::trim) {
        None => None,
        Some("default") => Some(None),
        Some(raw) => Some(Some(raw.parse::<u64>().map_err(|_| {
            anyhow::anyhow!("--interval takes minutes, 0 or default, not {raw:?}")
        })?)),
    };
    let password = if args.password_stdin {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let password = line.trim_end_matches(['\r', '\n']).to_owned();
        anyhow::ensure!(!password.is_empty(), "the password is empty");
        Some(password)
    } else {
        None
    };
    let change = holtmail_api::AccountChange {
        connection,
        jmap,
        password,
        sync_interval_minutes,
        min_age_days: args.min_age_days.map(|days| (days > 0).then_some(days)),
        contacts_calendars: args.contacts_calendars.as_deref().map(|on| {
            holtmail_api::ContactsCalendarsChange {
                enabled: on == "on",
                contacts_url: args.carddav_url.clone().or_else(|| args.dav_url.clone()),
                calendars_url: args.caldav_url.clone().or_else(|| args.dav_url.clone()),
            }
        }),
    };
    anyhow::ensure!(
        change.password.is_some()
            || crate::oauth::OAuthSettings::of(&account).is_some()
            || !accounts::moves_server(&account, &change),
        "changing the server needs the password or token again; pass it with --password-stdin"
    );
    let mut account = if change == holtmail_api::AccountChange::default() {
        account
    } else {
        let secrets = config::open_secrets(config)?;
        let oauth = config::open_oauth(config)?;
        accounts::configure(&*store, &*secrets, &oauth, &account, change)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?
    };
    if let Some(name) = args.name {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "the name is empty");
        account.name = name.to_owned();
    }
    if args.pause || args.resume {
        anyhow::ensure!(
            !(args.resume && account.kind == holtmail_store::AccountKind::Import),
            "an import account has no server"
        );
        account.enabled = args.resume;
    }
    store.update_account(&account).await?;
    println!("Changed {} ({}).", account.name, account.id);
    let skipped = if let Some(connection) = accounts::connection(&account) {
        println!(
            "  server    {}:{} ({}) as {}",
            connection.host, connection.port, connection.security, connection.username
        );
        Some(connection.exclude)
    } else if let Some(connection) = accounts::jmap_connection(&account) {
        println!(
            "  jmap      {} ({}{})",
            connection.url,
            connection.auth,
            if connection.username.is_empty() {
                String::new()
            } else {
                format!(" as {}", connection.username)
            }
        );
        Some(connection.exclude)
    } else {
        None
    };
    if let Some(skipped) = skipped {
        if !skipped.is_empty() {
            println!("  skipped   {}", skipped.join(", "));
        }
        println!(
            "  interval  {}",
            match accounts::sync_interval(&account) {
                None => "the server's".to_owned(),
                Some(0) => "only on request".to_owned(),
                Some(minutes) => format!("every {minutes} minutes"),
            }
        );
        println!(
            "  min age   {}",
            accounts::min_age_days(&account)
                .map_or_else(|| "none".to_owned(), |days| format!("{days} days"))
        );
    }
    if let Some(dav) = dav::details(&account) {
        println!(
            "  contacts  {}",
            match (dav.enabled, dav.needs_consent, dav.protocol) {
                (false, _, _) => "off".to_owned(),
                (true, true, _) => "on, waiting for Sign in again".to_owned(),
                (true, false, holtmail_api::ContactsProtocol::Graph) =>
                    "on, through Microsoft Graph".to_owned(),
                (true, false, holtmail_api::ContactsProtocol::Jmap) =>
                    "on, through the JMAP session".to_owned(),
                (true, false, holtmail_api::ContactsProtocol::Dav) => format!(
                    "on, {} and {}",
                    dav.contacts_url.unwrap_or_default(),
                    dav.calendars_url.unwrap_or_default()
                ),
            }
        );
    }
    println!("  {}", if account.enabled { "enabled" } else { "paused" });
    Ok(())
}

async fn sync_contacts(services: &Services, account: &holtmail_store::Account) {
    if !dav::DavSettings::of(account).is_some_and(|d| d.enabled) || !dav::available(account) {
        return;
    }
    if dav::needs_consent(account) {
        println!("  contacts and calendars wait for Sign in again");
        return;
    }
    let source = match dav::source_for(
        account,
        &*services.store,
        &*services.secrets,
        &services.oauth,
    )
    .await
    {
        Ok(source) => source,
        Err(error) => {
            println!("  contacts and calendars: {error:#}");
            return;
        }
    };
    let syncer = holtmail_pim_sync::PimSyncer::new(services.store.clone(), services.ingest());
    match syncer.sync(account.archive_id, account.id, &*source).await {
        Ok(report) => {
            for entry in &report.collections {
                let name = match services.store.get_collection(entry.collection).await {
                    Ok(Some(c)) => c.name,
                    _ => entry.collection.to_string(),
                };
                println!(
                    "  {:<40} {} new, {} changed, {} unchanged, {} removed, {} failed ({:?}){}",
                    name,
                    entry.new,
                    entry.changed,
                    entry.same,
                    entry.removed,
                    entry.failed,
                    entry.mode,
                    entry
                        .error
                        .as_ref()
                        .map(|e| format!(" — {e}"))
                        .unwrap_or_default()
                );
            }
            if report.removed_collections > 0 {
                println!(
                    "  {} address books or calendars marked removed",
                    report.removed_collections
                );
            }
        }
        Err(holtmail_pim_sync::PimSyncError::Source(error)) => {
            println!("  contacts and calendars: {}", dav::reason(&error));
        }
        Err(error) => println!("  contacts and calendars: {error}"),
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
    let schedule = Arc::new(
        schedule::Schedule::open(config.sync_interval_minutes, services.store.clone()).await?,
    );
    let runner =
        mailboxes::Runner::open(services.clone(), config.data_dir.join("uploads"), schedule)
            .await?;
    let mut app = holtmail_api::router(ApiState {
        blobs: services.blobs.clone(),
        store: services.store.clone(),
        index: services.index.clone(),
        auth: auth.clone(),
        mailboxes: Arc::new(runner.clone()),
        operations: Arc::new(runner.clone()),
        fetcher: services.fetcher.clone(),
        secure_cookies: config.https(),
    });
    tokio::spawn(runner.clone().write_tasks());
    tokio::spawn(services.hooks.clone().run());
    if let Some(smtp) = &config.smtp {
        let listener = smtp::start(smtp, &*services.store, services.ingest()).await?;
        let _ = services.smtp.set(listener);
    }
    runner.resume().await;
    let services_for_passes = services.clone();
    tokio::spawn(async {
        let services = services_for_passes;
        ledger::enter_at_start(services.store.clone()).await;
        reindex_if_needed(services.clone()).await;
        invitations_if_needed(services.clone()).await;
        directory_text_if_needed(services.clone()).await;
        people::build_if_needed(&*services.store).await;
    });
    tokio::spawn(runner.clone().run_schedule());
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
    .with_graceful_shutdown(shutdown())
    .await?;
    runner.flush_tasks().await;
    Ok(())
}

/// The setting that says the search index must be rebuilt from the store, set when an index of an
/// older version was replaced by an empty one.
pub(crate) const REINDEX_SETTING: &str = "index_rebuild";

/// The setting that says messages with attachments were reindexed with their attachments' text.
/// Archives from before attachment text was indexed lack it, and `serve` fills it in at start.
pub(crate) const ATTACHMENT_TEXT_SETTING: &str = "attachment_text";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    All,
    WithAttachments,
}

/// Rebuilds the index documents of the messages in `scope` from the store and the blobs,
/// reporting the count after each page. A full rebuild clears [`REINDEX_SETTING`]; both set
/// [`ATTACHMENT_TEXT_SETTING`]. Messages that fail are logged and skipped.
async fn reindex(
    store: &dyn Store,
    ingest: &holtmail_ingest::Ingest,
    scope: Scope,
    progress: impl Fn(u64),
) -> anyhow::Result<u64> {
    let mut done = 0u64;
    for archive in store.list_archives().await? {
        let mut after = None;
        loop {
            let page = store
                .list_messages(&holtmail_store::MessageQuery {
                    archive_id: archive.id,
                    folder_id: None,
                    label: None,
                    after,
                    limit: 500,
                })
                .await?;
            for message in &page.items {
                if scope == Scope::WithAttachments && message.attachment_count == 0 {
                    continue;
                }
                if let Err(error) = ingest.reindex(message.id).await {
                    tracing::warn!(message = %message.id, %error, "not reindexed");
                }
                done += 1;
            }
            ingest.commit().await?;
            progress(done);
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
    }
    if scope == Scope::All {
        let mut after = None;
        loop {
            let ids = store.item_ids(after, 500).await?;
            for id in &ids {
                if let Err(error) = ingest.rebuild_item(*id).await {
                    tracing::warn!(item = %id, %error, "contact or event not rebuilt");
                }
            }
            ingest.commit().await?;
            if ids.len() < 500 {
                break;
            }
            after = ids.last().copied();
        }
        store.set_setting(REINDEX_SETTING, None).await?;
    }
    store
        .set_setting(ATTACHMENT_TEXT_SETTING, Some("1"))
        .await?;
    Ok(done)
}

async fn reindex_if_needed(services: Arc<Services>) {
    let scope = match (
        services.store.setting(REINDEX_SETTING).await,
        services.store.setting(ATTACHMENT_TEXT_SETTING).await,
    ) {
        (Ok(Some(_)), _) => Scope::All,
        (Ok(None), Ok(None)) => Scope::WithAttachments,
        (Ok(None), Ok(Some(_))) => return,
        (Err(error), _) | (_, Err(error)) => {
            tracing::warn!(%error, "could not tell whether the index needs rebuilding");
            return;
        }
    };
    match scope {
        Scope::All => {
            tracing::info!("rebuilding the search index; search finds older mail as it goes")
        }
        Scope::WithAttachments => {
            tracing::info!("indexing the text of older attachments; search finds it as it goes")
        }
    }
    let started = std::time::Instant::now();
    match reindex(&*services.store, &services.ingest(), scope, |_| {}).await {
        Ok(done) => tracing::info!(
            messages = done,
            seconds = started.elapsed().as_secs(),
            "search index rebuilt"
        ),
        Err(error) => {
            tracing::warn!(%error, "search index not rebuilt; it is tried again at the next start")
        }
    }
}

/// The setting that says the calendar parts of every archived message were read into events.
/// Archives from before invitations were read lack them, and `serve` reads them once at start.
pub(crate) const INVITATIONS_SETTING: &str = "invitations_from_mail";

fn is_calendar(content_type: &str) -> bool {
    matches!(content_type, "text/calendar" | "application/ics")
}

/// Reads the calendar parts of every message that has one into events again, then sets
/// [`INVITATIONS_SETTING`]. Idempotent; messages that fail are logged and skipped. Returns how
/// many messages had calendar parts and how many versions were added.
pub(crate) async fn read_invitations(
    store: &dyn Store,
    ingest: &holtmail_ingest::Ingest,
) -> anyhow::Result<(u64, u64)> {
    let mut messages = 0u64;
    let mut versions = 0u64;
    for archive in store.list_archives().await? {
        let mut after = None;
        loop {
            let page = store
                .list_messages(&holtmail_store::MessageQuery {
                    archive_id: archive.id,
                    folder_id: None,
                    label: None,
                    after,
                    limit: 500,
                })
                .await?;
            for message in &page.items {
                let attachments = store.attachments(message.id).await?;
                if !attachments.iter().any(|a| is_calendar(&a.content_type)) {
                    continue;
                }
                messages += 1;
                match ingest.record_invitations(message.id).await {
                    Ok(added) => versions += u64::from(added),
                    Err(error) => {
                        tracing::warn!(message = %message.id, %error, "calendar parts not read")
                    }
                }
            }
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
    }
    ingest.commit().await?;
    store.set_setting(INVITATIONS_SETTING, Some("1")).await?;
    Ok((messages, versions))
}

async fn invitations_if_needed(services: Arc<Services>) {
    match services.store.setting(INVITATIONS_SETTING).await {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, "could not tell whether invitations were read");
            return;
        }
    }
    tracing::info!("reading invitations in older mail into events");
    let started = std::time::Instant::now();
    match read_invitations(&*services.store, &services.ingest()).await {
        Ok((messages, versions)) => tracing::info!(
            messages,
            versions,
            seconds = started.elapsed().as_secs(),
            "invitations read"
        ),
        Err(error) => {
            tracing::warn!(%error, "invitations not read; it is tried again at the next start")
        }
    }
}

pub(crate) const DIRECTORY_TEXT_SETTING: &str = "directory_text";

fn is_directory(attachment: &holtmail_store::Attachment) -> bool {
    let media = attachment
        .content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let extension = attachment
        .filename
        .as_deref()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase());
    matches!(
        media.as_str(),
        "text/calendar"
            | "application/ics"
            | "text/vcard"
            | "text/x-vcard"
            | "text/directory"
            | "text/x-vcalendar"
    ) || matches!(
        extension.as_deref(),
        Some("ics" | "ical" | "ifb" | "vcf" | "vcard" | "vcs")
    )
}

/// Reads calendar and vCard attachments of every message again, so their index text is their
/// words rather than their raw lines, then sets [`DIRECTORY_TEXT_SETTING`]. Idempotent; messages
/// that fail are logged and skipped. Returns how many messages changed.
pub(crate) async fn refresh_directory_text(
    store: &dyn Store,
    ingest: &holtmail_ingest::Ingest,
) -> anyhow::Result<u64> {
    let mut messages = 0u64;
    for archive in store.list_archives().await? {
        let mut after = None;
        loop {
            let page = store
                .list_messages(&holtmail_store::MessageQuery {
                    archive_id: archive.id,
                    folder_id: None,
                    label: None,
                    after,
                    limit: 500,
                })
                .await?;
            for message in page.items.iter().filter(|m| m.attachment_count > 0) {
                match ingest
                    .refresh_attachment_text(message.id, &is_directory)
                    .await
                {
                    Ok(0) => {}
                    Ok(_) => messages += 1,
                    Err(error) => {
                        tracing::warn!(message = %message.id, %error, "attachment text not read")
                    }
                }
            }
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        ingest.commit().await?;
    }
    store.set_setting(DIRECTORY_TEXT_SETTING, Some("1")).await?;
    Ok(messages)
}

async fn directory_text_if_needed(services: Arc<Services>) {
    match services.store.setting(DIRECTORY_TEXT_SETTING).await {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, "could not tell whether calendar attachments were read");
            return;
        }
    }
    let started = std::time::Instant::now();
    match refresh_directory_text(&*services.store, &services.ingest()).await {
        Ok(messages) => tracing::info!(
            messages,
            seconds = started.elapsed().as_secs(),
            "calendar and contact attachments read again"
        ),
        Err(error) => tracing::warn!(
            %error,
            "calendar and contact attachments not read; it is tried again at the next start"
        ),
    }
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("listening for SIGTERM");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
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
