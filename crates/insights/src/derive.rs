use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use holtmail_category::{
    Categorizer, CategoryInput, CategorySnapshot, CategorySource, KindTags, MessageKind,
    ReservationKind, ReservationKinds, Signal, Signals, TAXONOMY_VERSION, TopicSet, TrustState,
};
use holtmail_core::{AccountId, ArchiveId};
use holtmail_insight::{Confidence, InsightKind, Items};
use holtmail_store::{
    Account, AccountKind, CategoryPolicy, CategoryWrite, DeriveRow, MessageAlias, OverrideScope,
    Store, StoreError,
};
use serde::Deserialize;

/// The version of stage B's own rules: trust, coverage, precedence and the gates. Part of
/// [`DeriveContext::derive_fp`].
pub const DERIVE_VERSION: &str = "1";

/// The id of the categorizer that always votes, whatever [`DeriveConfig::sources`] lists.
pub const ALWAYS_ON_SOURCE: &str = "signals";

/// How many messages with an `Authentication-Results` field [`suggest_authserv`] reads, at most.
pub const SUGGEST_WINDOW: usize = 200;
/// How many messages with an `Authentication-Results` field [`suggest_authserv`] needs, at least.
pub const SUGGEST_MIN_MESSAGES: usize = 50;
/// The share, in percent, of those messages whose topmost field must carry the suggested id.
pub const SUGGEST_SHARE_PERCENT: usize = 95;

const AUTH_FACTS_VERSION: u32 = 2;
const MAX_KEY_BYTES: usize = 320;

/// Stage-B settings: the `[insights]`, `[insights.categories]` and `[smtp]` parts that change
/// derived rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeriveConfig {
    /// Whether the `health` topic may be derived. When `false` it is dropped from every source.
    pub health: bool,
    /// The ids of the categorizers that vote, besides [`ALWAYS_ON_SOURCE`], which always does.
    pub sources: Vec<String>,
    /// The trusted authserv-ids of import accounts other than the SMTP one.
    pub import_authserv_ids: Vec<String>,
    /// The trusted authserv-ids of the SMTP listener's account.
    pub smtp_authserv_ids: Vec<String>,
    /// The name of the import account the SMTP listener files mail into. Empty: no account is
    /// the SMTP one.
    pub smtp_account_name: String,
}

impl Default for DeriveConfig {
    /// Health on, only the `signals` categorizer, no trusted ids for imports and SMTP.
    fn default() -> Self {
        Self {
            health: true,
            sources: vec![ALWAYS_ON_SOURCE.to_owned()],
            import_authserv_ids: Vec::new(),
            smtp_authserv_ids: Vec::new(),
            smtp_account_name: String::new(),
        }
    }
}

/// Stage B of message insights: stored stage-A columns, the stored alias row, the trust lists and
/// the archive's own addresses turned into trust, kind and topics (`docs/INSIGHTS.md` §4.2, §4.3,
/// §6.5).
///
/// Guarantees:
///
/// - **No blob read.** Everything comes from a [`DeriveRow`] and a [`DeriveContext`].
/// - **Deterministic.** Equal rows under equal contexts give equal writes; no clock, randomness
///   or hash-map order reaches the result.
/// - **Reproducible.** Every input of [`Deriver::derive`] is covered by
///   [`DeriveContext::derive_fp`], so a row written under a fingerprint is what the rules give for
///   that fingerprint.
pub struct Deriver {
    categorizers: Vec<Arc<dyn Categorizer>>,
    config: DeriveConfig,
}

impl fmt::Debug for Deriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Deriver")
            .field(
                "categorizers",
                &self.categorizers.iter().map(|c| c.id()).collect::<Vec<_>>(),
            )
            .field("config", &self.config)
            .finish()
    }
}

/// Everything stage B reads besides the message: one snapshot of each voting categorizer, the
/// archive's trust lists and its own addresses, and the fingerprint they give.
///
/// Taken once per pass, or per message on ingest. It never changes after it is taken.
#[derive(Clone)]
pub struct DeriveContext {
    archive: ArchiveId,
    snapshots: Vec<(String, String, Arc<dyn CategorySnapshot>)>,
    trusted: BTreeMap<AccountId, Vec<String>>,
    own: BTreeSet<String>,
    health: bool,
    derive_fp: String,
}

impl fmt::Debug for DeriveContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeriveContext")
            .field("archive", &self.archive)
            .field("derive_fp", &self.derive_fp)
            .finish_non_exhaustive()
    }
}

impl DeriveContext {
    /// The archive it was taken for.
    pub fn archive(&self) -> ArchiveId {
        self.archive
    }

    /// The stage-B fingerprint: a hex BLAKE3 hash of [`DERIVE_VERSION`], each voting snapshot's
    /// `id=version` in categorizer order, [`holtmail_links::psl_version`],
    /// [`TAXONOMY_VERSION`], the `health` flag, the sorted source list, and hashes of the
    /// archive's effective trust lists (by account, sorted) and its own-address set (sorted).
    /// Changes whenever a derived row could differ; overrides are not part of it.
    pub fn derive_fp(&self) -> &str {
        &self.derive_fp
    }

    /// The effective trusted authserv-ids of `account`, lowercase, sorted and unique; empty for
    /// an account of another archive.
    pub fn trusted_authserv_ids(&self, account: AccountId) -> &[String] {
        self.trusted.get(&account).map_or(&[], Vec::as_slice)
    }

    /// Whether `address` is one of the archive's own addresses, after the normalisation
    /// [`override_key`] applies to an address (lowercase, A-label domain).
    pub fn is_own(&self, address: &str) -> bool {
        let address = normalize_address(address).unwrap_or_else(|| address.trim().to_lowercase());
        self.own.contains(&address)
    }
}

impl Deriver {
    /// A deriver over the categorizers that vote under `config`: [`ALWAYS_ON_SOURCE`] and those
    /// whose id is in [`DeriveConfig::sources`], in the given order. The order breaks ties
    /// between kinds of equal rank: the earlier categorizer wins.
    pub fn new(categorizers: Vec<Arc<dyn Categorizer>>, config: DeriveConfig) -> Self {
        let categorizers = categorizers
            .into_iter()
            .filter(|c| c.id() == ALWAYS_ON_SOURCE || config.sources.iter().any(|s| s == c.id()))
            .collect();
        Self {
            categorizers,
            config,
        }
    }

    /// The settings it derives under.
    pub fn config(&self) -> &DeriveConfig {
        &self.config
    }

    /// The policy the store applies overrides under, matching these settings.
    pub fn policy(&self) -> CategoryPolicy {
        CategoryPolicy {
            health: self.config.health,
        }
    }

    /// Takes a [`DeriveContext`] for `archive`: each voting categorizer's snapshot once, the
    /// archive's accounts with their trust lists, and its own addresses (account addresses and
    /// own alias addresses). Reads the store only; no blob.
    ///
    /// An import account's trusted list is [`DeriveConfig::smtp_authserv_ids`] when it is named
    /// [`DeriveConfig::smtp_account_name`], else [`DeriveConfig::import_authserv_ids`]; every other
    /// account's is the list the store holds for it.
    pub async fn context(
        &self,
        store: &dyn Store,
        archive: ArchiveId,
    ) -> Result<DeriveContext, StoreError> {
        let accounts = store.list_accounts(archive).await?;
        let stored = store.trusted_authserv_ids(archive).await?;
        let aliases = store.own_alias_addresses(archive).await?;
        Ok(self.context_from(archive, &accounts, &stored, &aliases))
    }

    /// [`Deriver::context`] from values already read: the archive's `accounts`, the trust lists
    /// the store holds (`stored`) and its own alias addresses. Takes each snapshot once.
    pub fn context_from(
        &self,
        archive: ArchiveId,
        accounts: &[Account],
        stored: &BTreeMap<AccountId, Vec<String>>,
        own_aliases: &[String],
    ) -> DeriveContext {
        let snapshots: Vec<_> = self
            .categorizers
            .iter()
            .map(|categorizer| {
                let snapshot = categorizer.snapshot();
                (categorizer.id().to_owned(), snapshot.version(), snapshot)
            })
            .collect();
        let trusted: BTreeMap<AccountId, Vec<String>> = accounts
            .iter()
            .filter(|account| account.archive_id == archive)
            .map(|account| {
                let ids = match account.kind {
                    AccountKind::Import
                        if !self.config.smtp_account_name.is_empty()
                            && account.name == self.config.smtp_account_name =>
                    {
                        self.config.smtp_authserv_ids.clone()
                    }
                    AccountKind::Import => self.config.import_authserv_ids.clone(),
                    _ => stored.get(&account.id).cloned().unwrap_or_default(),
                };
                (account.id, normalize_ids(ids))
            })
            .collect();
        let own: BTreeSet<String> = accounts
            .iter()
            .filter(|account| account.archive_id == archive)
            .filter_map(|account| account.address.as_deref())
            .chain(own_aliases.iter().map(String::as_str))
            .filter_map(|address| {
                normalize_address(address).or_else(|| {
                    let address = address.trim().to_lowercase();
                    (!address.is_empty()).then_some(address)
                })
            })
            .collect();

        let mut sources: Vec<&str> = self.config.sources.iter().map(String::as_str).collect();
        sources.sort_unstable();
        sources.dedup();
        let mut text = format!(
            "derive={DERIVE_VERSION}\npsl={}\ntaxonomy={TAXONOMY_VERSION}\nhealth={}\nsources={}\n",
            holtmail_links::psl_version(),
            self.config.health,
            sources.join(","),
        );
        for (id, version, _) in &snapshots {
            text.push_str(&format!("{id}={version}\n"));
        }
        let mut lists = String::new();
        for (account, ids) in &trusted {
            lists.push_str(&format!("{account}={}\n", ids.join(",")));
        }
        let mut owned = String::new();
        for address in &own {
            owned.push_str(address);
            owned.push('\n');
        }
        text.push_str(&format!(
            "trust={}\nown={}\n",
            blake3::hash(lists.as_bytes()).to_hex(),
            blake3::hash(owned.as_bytes()).to_hex()
        ));

        DeriveContext {
            archive,
            snapshots,
            trusted,
            own,
            health: self.config.health,
            derive_fp: blake3::hash(text.as_bytes()).to_hex().to_string(),
        }
    }

    /// Whether a voting categorizer's current snapshot has another version than the one in
    /// `ctx`, so a pass under `ctx` should stop and take a new context.
    pub fn changed(&self, ctx: &DeriveContext) -> bool {
        self.categorizers.len() != ctx.snapshots.len()
            || self.categorizers.iter().zip(&ctx.snapshots).any(
                |(categorizer, (id, version, _))| {
                    categorizer.id() != id || categorizer.snapshot().version() != *version
                },
            )
    }

    /// Stage B for one message under `ctx`. Pure: no I/O, deterministic, never panics, whatever
    /// the stored columns hold.
    ///
    /// - **Trust** (§4.3), first rule that applies: an alias row gives `relayed` when the service
    ///   signed it, else `unverified`, and never `aligned`; with no `Authentication-Results`
    ///   result from an id trusted for one of the message's accounts, `unknown`; else the topmost
    ///   trusted result alone decides `aligned` (exactly one From, a DKIM pass whose domain, or
    ///   `header.i` domain, has the From's registrable domain, at least one stored signature
    ///   matching it by `d`, `s` and `b` prefix, none with `l=`, no signature cap hit), else
    ///   `unverified`. Unreadable auth facts give `unknown`.
    /// - **One-click coverage** (§7.2) only when aligned: every signature matching a pass of
    ///   that result signs both `List-Unsubscribe` and `List-Unsubscribe-Post`, none has `l=`,
    ///   there is at least one, and the message has exactly one of each field.
    /// - **Sender**: the alias's real sender on an alias hit, else the From address; lowercase
    ///   with an A-label domain, and its registrable domain.
    /// - **Kind**: the vote of the highest [`CategorySource::rank`] that names one, else
    ///   `personal` from headers.
    /// - **Topics**: the union of every vote, each topic's source the highest rank naming it,
    ///   then [`TopicSet::gated`] by trust and the health flag.
    pub fn derive(&self, ctx: &DeriveContext, row: &DeriveRow) -> CategoryWrite {
        let facts = parse_facts(&row.auth);
        let trusted: BTreeSet<&str> = row
            .accounts
            .iter()
            .flat_map(|account| ctx.trusted_authserv_ids(*account))
            .map(String::as_str)
            .collect();
        let assessment = assess(facts.as_ref(), row.alias.as_ref(), &trusted);

        let from_address = row.from_address.as_deref().and_then(normalize_address);
        let sender_address = row
            .alias
            .as_ref()
            .and_then(|alias| alias.sender.as_ref())
            .and_then(|sender| normalize_address(&sender.email))
            .or_else(|| from_address.clone());
        let sender_domain = sender_address
            .as_deref()
            .and_then(|address| address.rsplit_once('@'))
            .and_then(|(_, domain)| holtmail_links::registrable_domain(domain));
        let own = from_address
            .as_deref()
            .is_some_and(|address| ctx.own.contains(address));

        let signals = Signals::from_bits(row.signals);
        let input = CategoryInput {
            signals,
            kinds: KindTags::from_bits(row.kinds),
            reservations: reservations(&row.items),
            structured: signals.contains(Signal::Structured),
            forwarded: signals.contains(Signal::ForwardMarker),
            own,
            trust: assessment.trust.clone(),
            sender_address: sender_address.clone(),
            sender_domain: sender_domain.clone(),
        };

        let mut kind: Option<(MessageKind, CategorySource)> = None;
        let mut topics = TopicSet::EMPTY;
        let mut sources: BTreeMap<_, CategorySource> = BTreeMap::new();
        for (_, _, snapshot) in &ctx.snapshots {
            let Some(vote) = snapshot.categorize(&input) else {
                continue;
            };
            if let Some(voted) = vote.kind
                && kind.is_none_or(|(_, source)| vote.kind_source.rank() > source.rank())
            {
                kind = Some((voted, vote.kind_source));
            }
            topics |= vote.topics;
            for topic in vote.topics.iter() {
                let source = sources.entry(topic).or_insert(vote.source);
                if vote.source.rank() > source.rank() {
                    *source = vote.source;
                }
            }
        }
        let (derived_kind, derived_kind_source) =
            kind.unwrap_or((MessageKind::Personal, CategorySource::Headers));
        let derived_topics = topics.gated(&assessment.trust, ctx.health);
        sources.retain(|topic, _| derived_topics.contains(*topic));

        CategoryWrite {
            message: row.message,
            archive: row.archive,
            derive_fp: ctx.derive_fp.clone(),
            trust: assessment.trust,
            trust_authserv: assessment.authserv,
            one_click_ok: assessment.one_click_ok,
            own,
            sender_address,
            sender_domain,
            derived_kind,
            derived_kind_source,
            derived_topics,
            derived_sources: sources,
        }
    }
}

fn normalize_ids(ids: Vec<String>) -> Vec<String> {
    let mut ids: Vec<String> = ids
        .into_iter()
        .map(|id| id.trim().to_ascii_lowercase())
        .filter(|id| !id.is_empty())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn reservations(items: &str) -> ReservationKinds {
    let Ok(read) = Items::from_json(items) else {
        return ReservationKinds::EMPTY;
    };
    read.items
        .items
        .iter()
        .filter(|item| item.confidence >= Confidence::Likely && !item.forwarded)
        .filter_map(|item| match &item.kind {
            InsightKind::Reservation(reservation) => Some(ReservationKind::from(reservation)),
            _ => None,
        })
        .collect()
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Facts {
    v: u32,
    results: Vec<ResultFact>,
    signatures: Vec<SignatureFact>,
    fields: BTreeMap<String, u64>,
    sig_cap_hit: bool,
    from_domain: Option<String>,
    from_addresses: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ResultFact {
    pos: u64,
    authserv: String,
    dkim: Vec<DkimFact>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DkimFact {
    d: Option<String>,
    i: Option<String>,
    s: Option<String>,
    b: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SignatureFact {
    d: Option<String>,
    s: Option<String>,
    b: Option<String>,
    h: BTreeMap<String, u64>,
    l: bool,
}

impl Facts {
    fn field(&self, name: &str) -> u64 {
        self.fields.get(name).copied().unwrap_or(0)
    }

    fn topmost(&self) -> impl Iterator<Item = &ResultFact> {
        let mut results: Vec<&ResultFact> = self.results.iter().collect();
        results.sort_by_key(|result| result.pos);
        results.into_iter()
    }
}

impl DkimFact {
    fn signing_domain(&self) -> Option<&str> {
        self.d
            .as_deref()
            .or_else(|| {
                self.i
                    .as_deref()
                    .and_then(|i| i.rsplit_once('@'))
                    .map(|(_, d)| d)
            })
            .filter(|domain| !domain.is_empty())
    }

    fn matches(&self, signature: &SignatureFact) -> bool {
        let Some(domain) = self.signing_domain() else {
            return false;
        };
        signature
            .d
            .as_deref()
            .is_some_and(|d| d.eq_ignore_ascii_case(domain))
            && self.s.as_deref().is_none_or(|s| {
                signature
                    .s
                    .as_deref()
                    .is_some_and(|signed| signed.eq_ignore_ascii_case(s))
            })
            && self.b.as_deref().is_none_or(|b| {
                signature
                    .b
                    .as_deref()
                    .is_some_and(|signed| signed.starts_with(b))
            })
    }
}

impl SignatureFact {
    fn signs(&self, name: &str) -> bool {
        self.h.get(name).is_some_and(|count| *count > 0)
    }
}

fn parse_facts(json: &str) -> Option<Facts> {
    let facts: Facts = serde_json::from_str(json).ok()?;
    (facts.v == AUTH_FACTS_VERSION).then_some(facts)
}

struct Assessment {
    trust: TrustState,
    authserv: Option<String>,
    one_click_ok: bool,
}

fn assess(
    facts: Option<&Facts>,
    alias: Option<&MessageAlias>,
    trusted: &BTreeSet<&str>,
) -> Assessment {
    let unknown = Assessment {
        trust: TrustState::Unknown,
        authserv: None,
        one_click_ok: false,
    };
    if let Some(alias) = alias {
        return Assessment {
            trust: if alias.signed {
                TrustState::Relayed {
                    provider: alias.provider.clone(),
                }
            } else {
                TrustState::Unverified
            },
            ..unknown
        };
    }
    let Some(facts) = facts else {
        return unknown;
    };
    let Some(result) = facts
        .topmost()
        .find(|result| trusted.contains(result.authserv.to_ascii_lowercase().as_str()))
    else {
        return unknown;
    };
    let authserv = Some(result.authserv.to_ascii_lowercase());

    let Some(domain) = aligned_domain(facts, result) else {
        return Assessment {
            trust: TrustState::Unverified,
            authserv,
            one_click_ok: false,
        };
    };
    Assessment {
        trust: TrustState::Aligned { domain },
        authserv,
        one_click_ok: covered(facts, result),
    }
}

fn aligned_domain(facts: &Facts, result: &ResultFact) -> Option<String> {
    if facts.sig_cap_hit || facts.field("from") != 1 || facts.from_addresses != 1 {
        return None;
    }
    let from = holtmail_links::registrable_domain(facts.from_domain.as_deref()?)?;
    result.dkim.iter().find_map(|dkim| {
        let domain = dkim.signing_domain()?;
        if holtmail_links::registrable_domain(domain)? != from {
            return None;
        }
        let matching: Vec<&SignatureFact> = facts
            .signatures
            .iter()
            .filter(|s| dkim.matches(s))
            .collect();
        (!matching.is_empty() && matching.iter().all(|signature| !signature.l))
            .then(|| domain.to_ascii_lowercase())
    })
}

fn covered(facts: &Facts, result: &ResultFact) -> bool {
    if facts.sig_cap_hit
        || facts.field("list-unsubscribe") != 1
        || facts.field("list-unsubscribe-post") != 1
    {
        return false;
    }
    let candidates: Vec<&SignatureFact> = facts
        .signatures
        .iter()
        .filter(|signature| result.dkim.iter().any(|dkim| dkim.matches(signature)))
        .collect();
    !candidates.is_empty()
        && candidates.iter().all(|signature| {
            !signature.l
                && signature.signs("list-unsubscribe")
                && signature.signs("list-unsubscribe-post")
        })
}

/// What the stored auth facts of one message say, for an operator's view: no trust decision.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthSummary {
    /// The authserv-id of every recorded `Authentication-Results` field, topmost first, as
    /// recorded.
    pub authserv_ids: Vec<String>,
    /// How many `From` fields the message has.
    pub from_fields: usize,
    /// How many `List-Unsubscribe` fields it has.
    pub unsubscribe_fields: usize,
    /// How many `List-Unsubscribe-Post` fields it has.
    pub unsubscribe_post_fields: usize,
}

/// The [`AuthSummary`] of a stored auth facts JSON. Lenient: unreadable or unknown JSON gives the
/// empty summary. Never panics.
pub fn auth_summary(auth_json: &str) -> AuthSummary {
    let Some(facts) = parse_facts(auth_json) else {
        return AuthSummary::default();
    };
    let count = |name| usize::try_from(facts.field(name)).unwrap_or(usize::MAX);
    AuthSummary {
        authserv_ids: facts.topmost().map(|r| r.authserv.clone()).collect(),
        from_fields: count("from"),
        unsubscribe_fields: count("list-unsubscribe"),
        unsubscribe_post_fields: count("list-unsubscribe-post"),
    }
}

/// The suggested trusted authserv-id of an account (§4.3) from the auth facts of its most
/// recently archived messages, most recent first, as `CategoryStore::recent_auth_facts` lists
/// them.
///
/// Of the first [`SUGGEST_WINDOW`] facts that record at least one `Authentication-Results`
/// field, the id of the topmost field, lowercase, when at least [`SUGGEST_MIN_MESSAGES`] such
/// facts exist and one id is topmost in at least [`SUGGEST_SHARE_PERCENT`] percent of them;
/// `None` otherwise. Unreadable facts are skipped. Deterministic. A suggestion is never trust:
/// only an operator's list is.
pub fn suggest_authserv(facts: &[String]) -> Option<String> {
    let tops: Vec<String> = facts
        .iter()
        .filter_map(|json| parse_facts(json))
        .filter_map(|facts| {
            facts
                .topmost()
                .next()
                .map(|r| r.authserv.to_ascii_lowercase())
        })
        .take(SUGGEST_WINDOW)
        .collect();
    if tops.len() < SUGGEST_MIN_MESSAGES {
        return None;
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for id in &tops {
        *counts.entry(id.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .find(|(id, count)| !id.is_empty() && count * 100 >= tops.len() * SUGGEST_SHARE_PERCENT)
        .map(|(id, _)| id.to_owned())
}

/// The normalised override key for `raw` under `scope` (§4.5), or `None` when `raw` is not one.
///
/// Trimmed, lowercase, the domain as an IDNA A-label without a trailing dot; a `+tag` is kept.
/// An address needs exactly one `@`, a non-empty local part and a domain with at least two
/// labels. A domain key is the registrable domain of the host; an address is accepted there too
/// and its domain taken. Deterministic; never panics.
pub fn override_key(scope: OverrideScope, raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_KEY_BYTES {
        return None;
    }
    match scope {
        OverrideScope::Address => {
            let (local, domain) = split_address(raw)?;
            let domain = ascii_domain(domain, true)?;
            Some(format!("{}@{domain}", local.to_lowercase()))
        }
        OverrideScope::Domain => {
            let host = if raw.contains('@') {
                split_address(raw)?.1
            } else {
                raw
            };
            holtmail_links::registrable_domain(&ascii_domain(host, true)?)
        }
    }
}

fn split_address(raw: &str) -> Option<(&str, &str)> {
    let (local, domain) = raw.split_once('@')?;
    if local.is_empty() || local.chars().any(char::is_whitespace) || domain.contains('@') {
        return None;
    }
    Some((local, domain))
}

fn ascii_domain(domain: &str, dotted: bool) -> Option<String> {
    let domain = domain.strip_suffix('.').unwrap_or(domain);
    if domain.is_empty() || domain.starts_with('[') {
        return None;
    }
    let domain = match url::Host::parse(domain).ok()? {
        url::Host::Domain(domain) => domain.to_ascii_lowercase(),
        _ => return None,
    };
    let valid = domain.len() <= 253
        && (!dotted || domain.contains('.'))
        && !domain.split('.').any(str::is_empty);
    valid.then_some(domain)
}

fn normalize_address(raw: &str) -> Option<String> {
    let (local, domain) = split_address(raw.trim())?;
    Some(format!(
        "{}@{}",
        local.to_lowercase(),
        ascii_domain(domain, false)?
    ))
}

#[cfg(test)]
mod tests;
