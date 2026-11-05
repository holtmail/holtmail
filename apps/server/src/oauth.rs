use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use holtmail_core::{AccountId, ArchiveId};
use holtmail_oauth::{Access, Client, OAuthError, Provider, TokenEndpoint, Tokens};
use holtmail_secret::SecretBox;
use holtmail_store::{Account, AccountKind, Store};
use serde::{Deserialize, Serialize};

pub const CALLBACK_PATH: &str = "/api/oauth/callback";

const PENDING_FOR: Duration = Duration::from_secs(10 * 60);
const MAX_PENDING: usize = 100;
const REFRESH_BEFORE: Duration = Duration::from_secs(120);

/// What `settings.oauth` holds for an account that signs in with OAuth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthSettings {
    pub provider: String,
    #[serde(default)]
    pub needs_sign_in: bool,
}

impl OAuthSettings {
    pub fn of(account: &Account) -> Option<Self> {
        serde_json::from_value(account.settings.get("oauth")?.clone()).ok()
    }

    pub fn provider(&self) -> Option<Provider> {
        Provider::parse(&self.provider)
    }

    pub fn write(&self, settings: &mut serde_json::Value) {
        if !settings.is_object() {
            *settings = serde_json::json!({});
        }
        settings["oauth"] = serde_json::to_value(self).expect("plain settings serialize");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    NewAccount {
        archive: ArchiveId,
        name: Option<String>,
    },
    SignInAgain(AccountId),
}

pub struct Pending {
    pub provider: Provider,
    pub access: Access,
    pub email: String,
    pub purpose: Purpose,
    pub return_to: String,
    verifier: String,
    expires: Instant,
}

pub struct Started {
    pub url: String,
    pub state: String,
    pub needs_paste: bool,
    pub thunderbird: bool,
    pub expires_in: Duration,
}

struct Cached {
    token: String,
    until: Instant,
}

/// The OAuth clients, sign-ins waiting for their answer, and access tokens, all in memory.
pub struct OAuth {
    clients: HashMap<Provider, Client>,
    endpoint: Arc<dyn TokenEndpoint>,
    pending: Mutex<HashMap<String, Pending>>,
    tokens: Mutex<HashMap<AccountId, Cached>>,
}

impl OAuth {
    pub fn new(clients: Vec<Client>, endpoint: Arc<dyn TokenEndpoint>) -> Self {
        Self {
            clients: clients.into_iter().map(|c| (c.provider, c)).collect(),
            endpoint,
            pending: Mutex::new(HashMap::new()),
            tokens: Mutex::new(HashMap::new()),
        }
    }

    fn client(&self, provider: Provider, access: Access) -> Result<Client, OAuthError> {
        self.clients
            .get(&provider)
            .expect("every provider has a client")
            .for_access(access)
            .ok_or_else(|| {
                OAuthError::Rejected(format!(
                    "{} offers no {} access",
                    provider.name(),
                    access.as_str()
                ))
            })
    }

    pub fn start(
        &self,
        provider: Provider,
        access: Access,
        email: &str,
        purpose: Purpose,
        return_to: String,
    ) -> Result<Started, OAuthError> {
        let client = self.client(provider, access)?;
        let authorization = client.authorize(Some(email))?;
        let now = Instant::now();
        let mut pending = self.pending.lock().expect("pending lock");
        pending.retain(|_, p| p.expires > now);
        if pending.len() >= MAX_PENDING
            && let Some(oldest) = pending
                .iter()
                .min_by_key(|(_, p)| p.expires)
                .map(|(k, _)| k.clone())
        {
            pending.remove(&oldest);
        }
        pending.insert(
            authorization.state.clone(),
            Pending {
                provider,
                access,
                email: email.to_owned(),
                purpose,
                return_to,
                verifier: authorization.verifier,
                expires: now + PENDING_FOR,
            },
        );
        Ok(Started {
            url: authorization.url,
            state: authorization.state,
            needs_paste: !client.redirect_url.ends_with(CALLBACK_PATH),
            thunderbird: client.is_thunderbird(),
            expires_in: PENDING_FOR,
        })
    }

    /// The page a waiting sign-in goes back to, without using it up.
    pub fn return_to(&self, state: &str) -> Option<String> {
        let pending = self.pending.lock().expect("pending lock");
        pending
            .get(state)
            .filter(|p| p.expires > Instant::now())
            .map(|p| p.return_to.clone())
    }

    /// Removes and returns the waiting sign-in for `state`; each answer is used once.
    pub fn take(&self, state: &str) -> Option<Pending> {
        self.pending
            .lock()
            .expect("pending lock")
            .remove(state)
            .filter(|p| p.expires > Instant::now())
    }

    pub async fn exchange(&self, pending: &Pending, code: &str) -> Result<Tokens, OAuthError> {
        self.client(pending.provider, pending.access)?
            .exchange(&*self.endpoint, code, &pending.verifier)
            .await
    }

    pub fn remember(&self, account: AccountId, tokens: &Tokens) {
        self.tokens.lock().expect("tokens lock").insert(
            account,
            Cached {
                token: tokens.access_token.clone(),
                until: Instant::now() + tokens.expires_in,
            },
        );
    }

    pub fn forget(&self, account: AccountId) {
        self.tokens.lock().expect("tokens lock").remove(&account);
    }

    /// A fresh access token for an OAuth account: the cached one while it lives, else one from
    /// its sealed refresh token. A rotated refresh token is sealed and stored. A refresh token
    /// the provider no longer accepts marks the account as needing sign-in, which stops syncs.
    pub async fn access_token(
        &self,
        account: &Account,
        store: &dyn Store,
        secrets: &dyn SecretBox,
    ) -> anyhow::Result<String> {
        let settings = OAuthSettings::of(account).context("the account does not use OAuth")?;
        let provider = settings
            .provider()
            .with_context(|| format!("unknown OAuth provider {:?}", settings.provider))?;
        if settings.needs_sign_in {
            bail!(sign_in_again(provider));
        }
        if let Some(cached) = self.tokens.lock().expect("tokens lock").get(&account.id)
            && cached.until > Instant::now() + REFRESH_BEFORE
        {
            return Ok(cached.token.clone());
        }
        let context = account.id.to_string();
        let sealed = account
            .sealed_secret
            .as_deref()
            .context("the account has no stored sign-in")?;
        let refresh = String::from_utf8(
            secrets
                .open(&context, sealed)
                .context("the stored sign-in cannot be opened with this secret key")?,
        )
        .context("the stored sign-in is damaged")?;
        let client = self.client(provider, access_for(account.kind))?;
        match client.refresh(&*self.endpoint, &refresh).await {
            Ok(tokens) => {
                if let Some(rotated) = tokens.refresh_token.as_deref().filter(|t| *t != refresh) {
                    let mut stored = store
                        .get_account(account.id)
                        .await?
                        .context("the account was removed")?;
                    stored.sealed_secret = Some(secrets.seal(&context, rotated.as_bytes())?);
                    store.update_account(&stored).await?;
                }
                self.remember(account.id, &tokens);
                Ok(tokens.access_token)
            }
            Err(OAuthError::Revoked(_)) => {
                self.forget(account.id);
                if let Some(mut stored) = store.get_account(account.id).await? {
                    let mut settings = OAuthSettings::of(&stored).unwrap_or(settings);
                    settings.needs_sign_in = true;
                    settings.write(&mut stored.settings);
                    store.update_account(&stored).await?;
                }
                tracing::warn!(account = %account.id, provider = provider.as_str(), "OAuth sign-in revoked");
                Err(anyhow!(sign_in_again(provider)))
            }
            Err(error) => Err(anyhow!(
                "Could not get access from {}: {error}",
                provider.name()
            )),
        }
    }
}

/// What an account's OAuth sign-in grants: Graph for Graph accounts, IMAP for the rest.
pub fn access_for(kind: AccountKind) -> Access {
    match kind {
        AccountKind::Graph => Access::Graph,
        _ => Access::Imap,
    }
}

pub fn sign_in_again(provider: Provider) -> String {
    format!(
        "{} no longer accepts the sign-in for this account. Sign in again.",
        provider.name()
    )
}

#[cfg(test)]
mod tests {
    use holtmail_oauth::testing::FakeTokenEndpoint;
    use holtmail_secret_local::LocalSecretBox;
    use holtmail_store::{AccountKind, ArchiveStore, NewAccount};
    use holtmail_store_sqlite::SqliteStore;

    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: SqliteStore,
        secrets: LocalSecretBox,
        endpoint: Arc<FakeTokenEndpoint>,
        oauth: OAuth,
        account: Account,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("db")).await.unwrap();
        let secrets = LocalSecretBox::from_key([5; 32]);
        let archive = store.create_archive("Personal").await.unwrap();
        let mut settings = serde_json::json!({
            "host": "imap.gmail.com",
            "username": "ada@example.com",
        });
        OAuthSettings {
            provider: "google".into(),
            needs_sign_in: false,
        }
        .write(&mut settings);
        let mut account = store
            .create_account(NewAccount {
                archive_id: archive.id,
                kind: AccountKind::Imap,
                name: "Ada".into(),
                address: Some("ada@example.com".into()),
                settings,
                sealed_secret: None,
                enabled: true,
            })
            .await
            .unwrap();
        account.sealed_secret = Some(secrets.seal(&account.id.to_string(), b"refresh-1").unwrap());
        store.update_account(&account).await.unwrap();
        let endpoint = Arc::new(FakeTokenEndpoint::default());
        let oauth = OAuth::new(
            Provider::ALL.into_iter().map(Client::thunderbird).collect(),
            endpoint.clone(),
        );
        Fixture {
            _dir: dir,
            store,
            secrets,
            endpoint,
            oauth,
            account,
        }
    }

    impl Fixture {
        async fn token(&self) -> anyhow::Result<String> {
            let account = self.store.get_account(self.account.id).await?.unwrap();
            self.oauth
                .access_token(&account, &self.store, &self.secrets)
                .await
        }

        async fn sealed(&self) -> Vec<u8> {
            let account = self
                .store
                .get_account(self.account.id)
                .await
                .unwrap()
                .unwrap();
            self.secrets
                .open(&account.id.to_string(), &account.sealed_secret.unwrap())
                .unwrap()
        }
    }

    #[tokio::test]
    async fn refreshes_once_and_caches_the_access_token() {
        let f = fixture().await;
        f.endpoint.tokens("access-1", None, 3600);
        assert_eq!(f.token().await.unwrap(), "access-1");
        assert_eq!(f.token().await.unwrap(), "access-1");
        assert_eq!(f.endpoint.requests().len(), 1);
        assert_eq!(
            f.endpoint.last("refresh_token").as_deref(),
            Some("refresh-1")
        );

        f.endpoint.tokens("access-2", Some("refresh-2"), 60);
        f.oauth.forget(f.account.id);
        assert_eq!(f.token().await.unwrap(), "access-2");
        assert_eq!(
            f.sealed().await,
            b"refresh-2",
            "the rotated token was not stored"
        );
        f.endpoint.tokens("access-3", None, 3600);
        assert_eq!(
            f.token().await.unwrap(),
            "access-3",
            "a token about to expire was reused"
        );
    }

    #[tokio::test]
    async fn a_revoked_sign_in_stops_until_signed_in_again() {
        let f = fixture().await;
        f.endpoint.error("invalid_grant");
        let error = f.token().await.unwrap_err().to_string();
        assert!(error.contains("Sign in again"), "{error}");
        let stored = f.store.get_account(f.account.id).await.unwrap().unwrap();
        assert!(OAuthSettings::of(&stored).unwrap().needs_sign_in);
        assert_eq!(stored.settings["username"], "ada@example.com");
        assert!(f.token().await.is_err());
        assert_eq!(
            f.endpoint.requests().len(),
            1,
            "a revoked token was retried"
        );
    }

    #[tokio::test]
    async fn graph_accounts_refresh_for_graph() {
        let f = fixture().await;
        let mut settings = serde_json::json!({});
        OAuthSettings {
            provider: "microsoft".into(),
            needs_sign_in: false,
        }
        .write(&mut settings);
        let mut graph = f
            .store
            .create_account(NewAccount {
                archive_id: f.account.archive_id,
                kind: AccountKind::Graph,
                name: "Ada".into(),
                address: Some("ada@example.com".into()),
                settings,
                sealed_secret: None,
                enabled: true,
            })
            .await
            .unwrap();
        graph.sealed_secret = Some(f.secrets.seal(&graph.id.to_string(), b"refresh-g").unwrap());
        f.store.update_account(&graph).await.unwrap();
        f.endpoint.tokens("access-g", None, 3600);
        let token = f
            .oauth
            .access_token(&graph, &f.store, &f.secrets)
            .await
            .unwrap();
        assert_eq!(token, "access-g");
        assert!(
            f.endpoint
                .last("scope")
                .is_some_and(|s| s.contains("graph.microsoft.com/Mail.Read")),
            "a Graph account refreshed for IMAP"
        );

        let started = f
            .oauth
            .start(
                Provider::Microsoft,
                Access::Graph,
                "ada@example.com",
                Purpose::SignInAgain(graph.id),
                "/settings".into(),
            )
            .unwrap();
        assert!(started.url.contains("Mail.Read"));
        assert_eq!(f.oauth.take(&started.state).unwrap().access, Access::Graph);
        assert!(
            f.oauth
                .start(
                    Provider::Google,
                    Access::Graph,
                    "ada@example.com",
                    Purpose::SignInAgain(graph.id),
                    "/settings".into(),
                )
                .is_err()
        );
    }

    #[tokio::test]
    async fn other_failures_keep_the_account() {
        let f = fixture().await;
        f.endpoint.unreachable();
        assert!(f.token().await.is_err());
        let stored = f.store.get_account(f.account.id).await.unwrap().unwrap();
        assert!(!OAuthSettings::of(&stored).unwrap().needs_sign_in);
        assert_eq!(f.sealed().await, b"refresh-1");
    }

    #[test]
    fn waiting_sign_ins_are_used_once() {
        let oauth = OAuth::new(
            Provider::ALL.into_iter().map(Client::thunderbird).collect(),
            Arc::new(FakeTokenEndpoint::default()),
        );
        let archive = ArchiveId::new();
        let started = oauth
            .start(
                Provider::Microsoft,
                Access::Imap,
                "ada@example.com",
                Purpose::NewAccount {
                    archive,
                    name: None,
                },
                "/settings".into(),
            )
            .unwrap();
        assert!(started.needs_paste && started.thunderbird);
        assert!(started.url.contains("login_hint=ada%40example.com"));
        assert_eq!(
            oauth.return_to(&started.state).as_deref(),
            Some("/settings")
        );
        assert!(oauth.take("unknown").is_none());
        let pending = oauth.take(&started.state).unwrap();
        assert_eq!(
            pending.purpose,
            Purpose::NewAccount {
                archive,
                name: None
            }
        );
        assert!(oauth.take(&started.state).is_none());
        assert!(oauth.return_to(&started.state).is_none());
    }

    #[test]
    fn own_clients_need_no_paste() {
        let oauth = OAuth::new(
            vec![
                Client::own(
                    Provider::Google,
                    "own",
                    None,
                    "https://mail.example.com/api/oauth/callback",
                ),
                Client::thunderbird(Provider::Microsoft),
            ],
            Arc::new(FakeTokenEndpoint::default()),
        );
        let started = oauth
            .start(
                Provider::Google,
                Access::Imap,
                "ada@example.com",
                Purpose::SignInAgain(AccountId::new()),
                "/settings".into(),
            )
            .unwrap();
        assert!(!started.needs_paste && !started.thunderbird);
    }
}
