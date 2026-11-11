//! Rules in the server: the ingest observer that hands live mail to the executor once `serve`
//! started it, and the hooks that tell webhooks about paused flows and rule actions.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use holtmail_api::{RuleHooks, Rules};
use holtmail_core::{ArchiveId, FlowId, FlowRunId, MessageId, WebhookId};
use holtmail_hooks::Event;

use crate::webhooks::Hooks;

/// Hands every live message ingest adds to the rules executor, once [`RulesObserver::start`]
/// set it; before that (command-line tools, imports) nothing happens.
#[derive(Default)]
pub struct RulesObserver(OnceLock<Arc<Rules>>);

impl RulesObserver {
    /// Starts handing messages to `rules`. Later calls change nothing.
    pub fn start(&self, rules: Arc<Rules>) {
        let _ = self.0.set(rules);
    }
}

#[async_trait]
impl holtmail_ingest::Observer for RulesObserver {
    async fn added(
        &self,
        archive: ArchiveId,
        message: MessageId,
        _raw: &[u8],
        parsed: &holtmail_mime::ParsedMessage,
        placement: &holtmail_ingest::Placement,
    ) {
        if let Some(rules) = self.0.get() {
            rules
                .observe(archive, message, parsed, placement.live)
                .await;
        }
    }
}

/// [`RuleHooks`] over the server's webhooks.
pub struct WebhookRuleHooks(pub Arc<Hooks>);

#[async_trait]
impl RuleHooks for WebhookRuleHooks {
    async fn flow_paused(&self, archive: ArchiveId, flow: FlowId, reason: &str) {
        self.0
            .emit(Event::FlowPaused {
                flow_id: flow.to_string(),
                archive_id: archive,
                reason: reason.to_owned(),
            })
            .await;
    }

    async fn notified(&self, archive: ArchiveId, flow: FlowId, run: FlowRunId) {
        self.0
            .emit(Event::FlowNotified {
                flow_id: flow.to_string(),
                archive_id: archive,
                run_id: run.to_string(),
            })
            .await;
    }

    async fn webhook_allowed(&self, hook: &str) -> Result<(), String> {
        let id: WebhookId = hook
            .parse()
            .map_err(|_| "There is no such webhook.".to_owned())?;
        self.0
            .rule_target(id)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn call(
        &self,
        hook: &str,
        archive: ArchiveId,
        flow: FlowId,
        run: FlowRunId,
        message: Option<MessageId>,
    ) -> Result<(), String> {
        let id: WebhookId = hook
            .parse()
            .map_err(|_| "There is no such webhook.".to_owned())?;
        self.0
            .send_to(
                id,
                Event::FlowRun {
                    flow_id: flow.to_string(),
                    archive_id: archive,
                    run_id: run.to_string(),
                    message_id: message.map(|m| m.to_string()),
                },
            )
            .await
            .map_err(|error| error.to_string())
    }
}

/// What `holtmail rules` was asked to do.
pub enum RulesArgs {
    List {
        archive: Option<String>,
    },
    Test {
        flow: String,
        message: Option<String>,
        file: Option<std::path::PathBuf>,
        not_live: bool,
    },
    Export {
        flow: String,
        version: Option<u32>,
    },
    Import {
        archive: Option<String>,
        into: Option<String>,
        name: Option<String>,
        user: Option<String>,
        file: std::path::PathBuf,
    },
}

fn read_flow(path: &std::path::Path) -> anyhow::Result<holtmail_rules::Flow> {
    use anyhow::Context;
    let text = if path.as_os_str() == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    };
    let flow = holtmail_rules::Flow::from_json(&text).context("the file is not flow JSON")?;
    let mut problems: Vec<String> = match holtmail_rules::validate(&flow) {
        Ok(()) => Vec::new(),
        Err(issues) => issues
            .iter()
            .map(|i| format!("{}: {}", i.node.as_deref().unwrap_or("flow"), i.detail))
            .collect(),
    };
    for (node, query) in flow.queries() {
        if let Err(error) = holtmail_query::parse(query) {
            problems.push(format!("{node}: query: {error}"));
        }
    }
    anyhow::ensure!(
        problems.is_empty(),
        "the flow is not valid:\n  {}",
        problems.join("\n  ")
    );
    Ok(flow)
}

async fn flow_record(
    store: &dyn holtmail_store::Store,
    raw: &str,
) -> anyhow::Result<holtmail_store::FlowRecord> {
    use anyhow::Context;
    let id: FlowId = raw
        .parse()
        .map_err(|_| anyhow::anyhow!("{raw:?} is not a flow id"))?;
    store.get_flow(id).await?.context("There is no such flow.")
}

/// `holtmail rules`: lists flows, dry-runs one on a message, prints a version's JSON or imports
/// a flow from a file. Works while `serve` runs; an imported flow starts in test mode.
pub async fn cli(config: &crate::config::Config, args: RulesArgs) -> anyhow::Result<()> {
    use anyhow::Context;
    let store = crate::config::open_store(config).await?;
    match args {
        RulesArgs::List { archive } => {
            let archives = match archive {
                Some(name) => vec![crate::export::find_archive(&*store, &name).await?],
                None => store
                    .list_archives()
                    .await?
                    .into_iter()
                    .map(|a| a.id)
                    .collect(),
            };
            for archive in archives {
                for flow in store.list_flows(archive).await? {
                    println!(
                        "{}  archive {}  #{}  {}{}  v{}  {}  {}",
                        flow.id,
                        archive,
                        flow.position,
                        flow.mode.as_str(),
                        flow.paused
                            .as_ref()
                            .map(|p| format!(" (paused: {})", p.reason))
                            .unwrap_or_default(),
                        flow.version,
                        flow.trigger,
                        flow.name
                    );
                }
            }
            Ok(())
        }
        RulesArgs::Export { flow, version } => {
            let record = flow_record(&*store, &flow).await?;
            let stored = store
                .flow_version(record.id, version.unwrap_or(record.version))
                .await?
                .context("There is no such version.")?;
            let flow = holtmail_rules::Flow::from_json(&stored.definition)
                .context("the stored flow does not read")?;
            println!("{}", flow.to_json());
            Ok(())
        }
        RulesArgs::Test {
            flow,
            message,
            file,
            not_live,
        } => {
            let blobs = crate::config::open_blobs(config).await?;
            let record = flow_record(&*store, &flow).await?;
            let definition = match file {
                Some(path) => read_flow(&path)?,
                None => {
                    let stored = store
                        .flow_version(record.id, record.version)
                        .await?
                        .context("There is no such version.")?;
                    holtmail_rules::Flow::from_json(&stored.definition)
                        .context("the stored flow does not read")?
                }
            };
            let message = match message {
                Some(raw) => {
                    let id: MessageId = raw
                        .parse()
                        .map_err(|_| anyhow::anyhow!("{raw:?} is not a message id"))?;
                    let message = store
                        .get_message(id)
                        .await?
                        .context("There is no such message.")?;
                    anyhow::ensure!(
                        message.archive_id == record.archive_id,
                        "The message is in another archive."
                    );
                    Some(message)
                }
                None => None,
            };
            let plan = holtmail_api::dry_run(
                &*store,
                &*blobs,
                &record,
                &definition,
                message.as_ref(),
                !not_live,
                holtmail_core::Timestamp::now(),
            )
            .await
            .map_err(|reason| anyhow::anyhow!(reason))?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
            Ok(())
        }
        RulesArgs::Import {
            archive,
            into,
            name,
            user,
            file,
        } => {
            let flow = read_flow(&file)?;
            let saver = match user {
                Some(email) => store
                    .find_user_by_email(&email)
                    .await?
                    .context("There is no user with that email.")?,
                None => store
                    .list_users()
                    .await?
                    .into_iter()
                    .find(|u| u.role == holtmail_store::InstanceRole::Owner)
                    .context("There is no owner yet; pass --user.")?,
            };
            let now = holtmail_core::Timestamp::now();
            match into {
                Some(raw) => {
                    let record = flow_record(&*store, &raw).await?;
                    match store
                        .save_flow(
                            record.id,
                            record.version,
                            &flow.to_json(),
                            &flow.trigger.kind,
                            saver.id,
                            now,
                        )
                        .await?
                    {
                        holtmail_store::FlowSave::Saved(saved) => {
                            println!("{} saved as version {}", saved.id, saved.version)
                        }
                        holtmail_store::FlowSave::Stale { current } => {
                            anyhow::bail!(
                                "The flow was saved meanwhile; version {current} is current."
                            )
                        }
                    }
                }
                None => {
                    let archive = archive.context("Pass --archive for a new flow.")?;
                    let archive = crate::export::find_archive(&*store, &archive).await?;
                    let name = name.unwrap_or_else(|| {
                        file.file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "Imported flow".into())
                    });
                    let created = store
                        .create_flow(&holtmail_store::NewFlow {
                            archive_id: archive,
                            name,
                            definition: flow.to_json(),
                            trigger: flow.trigger.kind.clone(),
                            saved_by: saver.id,
                            at: now,
                        })
                        .await?;
                    println!("{} created in test mode", created.id);
                }
            }
            store
                .append_audit(&holtmail_store::NewAuditEntry {
                    at: now,
                    user: Some(saver.id),
                    kind: holtmail_store::AuditKind::RuleChanged,
                    archive: None,
                    account: None,
                    targets: vec!["imported".into()],
                    count: 1,
                })
                .await?;
            Ok(())
        }
    }
}
