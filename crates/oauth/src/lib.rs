//! Signing in to Gmail and Microsoft mailboxes with OAuth 2.0: provider presets, PKCE, the
//! authorize address, reading the answer the provider redirects to, and getting and refreshing
//! tokens.
//!
//! Talking to the provider's token endpoint goes through the [`TokenEndpoint`] contract, so
//! everything here is tested without a network.

mod redirect;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub use redirect::{Redirect, read_redirect};

/// Where Thunderbird's installed-app clients send the browser after sign-in. Nothing listens
/// there, so the browser shows an error page whose address holds the answer.
pub const LOOPBACK_REDIRECT: &str = "http://127.0.0.1";

const GOOGLE_CLIENT_ID: &str =
    "406964657835-aq8lmia8j95dhl1a2bvharmfk3t1hgqj.apps.googleusercontent.com";
const GOOGLE_CLIENT_SECRET: &str = "kSmqreRr0qwBWJgbf5Y-PjSU";
const MICROSOFT_CLIENT_ID: &str = "9e5f94bc-e8a4-4e73-b8be-63364c29d753";

/// When a token response says nothing about its lifetime.
const DEFAULT_LIFETIME: Duration = Duration::from_secs(3600);

/// A mail provider that signs in with OAuth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    /// Gmail and Google Workspace.
    Google,
    /// Outlook.com, Hotmail and Microsoft 365.
    Microsoft,
}

impl Provider {
    /// Every provider.
    pub const ALL: [Provider; 2] = [Provider::Google, Provider::Microsoft];

    /// `google` or `microsoft`, as stored and used in the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Microsoft => "microsoft",
        }
    }

    /// Parses [`Provider::as_str`] output.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "google" => Some(Self::Google),
            "microsoft" => Some(Self::Microsoft),
            _ => None,
        }
    }

    /// The name people know.
    pub fn name(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::Microsoft => "Microsoft",
        }
    }

    /// The provider of a well-known address domain, e.g. `gmail.com` or `outlook.com`. Custom
    /// domains hosted by either provider return `None`.
    pub fn for_address(address: &str) -> Option<Self> {
        let domain = address.rsplit_once('@')?.1.trim().to_ascii_lowercase();
        match domain.as_str() {
            "gmail.com" | "googlemail.com" => Some(Self::Google),
            "outlook.com" | "hotmail.com" | "live.com" | "msn.com" | "office365.com" => {
                Some(Self::Microsoft)
            }
            _ => None,
        }
    }

    /// The IMAP server host and its TLS port.
    pub fn imap(self) -> (&'static str, u16) {
        match self {
            Self::Google => ("imap.gmail.com", 993),
            Self::Microsoft => ("outlook.office365.com", 993),
        }
    }

    fn authorize_endpoint(self) -> &'static str {
        match self {
            Self::Google => "https://accounts.google.com/o/oauth2/auth",
            Self::Microsoft => "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        }
    }

    fn token_endpoint(self) -> &'static str {
        match self {
            Self::Google => "https://www.googleapis.com/oauth2/v3/token",
            Self::Microsoft => "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        }
    }

    /// The scopes a sign-in for `access` asks for, with a refresh token; `None` when the
    /// provider does not offer that access (Google has no Graph).
    pub fn scope(self, access: Access) -> Option<&'static str> {
        match (self, access) {
            (Self::Google, Access::Imap) => Some("https://mail.google.com/"),
            (Self::Google, Access::Graph) => None,
            (Self::Microsoft, Access::Imap) => {
                Some("https://outlook.office.com/IMAP.AccessAsUser.All offline_access")
            }
            (Self::Microsoft, Access::Graph) => Some(
                "https://graph.microsoft.com/Mail.Read https://graph.microsoft.com/User.Read offline_access",
            ),
            (Self::Google, Access::ImapDav) => Some(
                "https://mail.google.com/ https://www.googleapis.com/auth/carddav https://www.googleapis.com/auth/calendar.readonly",
            ),
            (Self::Microsoft, Access::ImapDav) => None,
            (Self::Microsoft, Access::GraphPim) => Some(
                "https://graph.microsoft.com/Mail.Read https://graph.microsoft.com/User.Read https://graph.microsoft.com/Contacts.Read https://graph.microsoft.com/Calendars.Read offline_access",
            ),
            (Self::Google, Access::GraphPim) => None,
        }
    }

    /// The scope a sign-in for `access` adds to also send mail, or `None` when its scopes
    /// already cover sending or there is no such access: Microsoft needs `SMTP.Send` for SMTP
    /// beside IMAP and `Mail.Send` for Graph; Google's mail scope covers SMTP.
    pub fn send_scope(self, access: Access) -> Option<&'static str> {
        match (self, access) {
            (Self::Microsoft, Access::Imap) => Some("https://outlook.office.com/SMTP.Send"),
            (Self::Microsoft, Access::Graph | Access::GraphPim) => {
                Some("https://graph.microsoft.com/Mail.Send")
            }
            _ => None,
        }
    }

    /// The scope a sign-in for `access` adds to also change mail on the server (flags, moves,
    /// deletes, appends), or `None` when its scopes already cover that: Microsoft Graph needs
    /// `Mail.ReadWrite`; Microsoft's IMAP scope and Google's mail scope already write.
    pub fn write_scope(self, access: Access) -> Option<&'static str> {
        match (self, access) {
            (Self::Microsoft, Access::Graph | Access::GraphPim) => {
                Some("https://graph.microsoft.com/Mail.ReadWrite")
            }
            _ => None,
        }
    }

    /// The scopes a sign-in for `access` adds to also change contacts and calendars upstream
    /// (`pim.edit`), or `None` when its scopes already cover that or it reads none: Microsoft
    /// Graph needs `Contacts.ReadWrite` and `Calendars.ReadWrite`; Google's CalDAV needs
    /// `auth/calendar` in place of `auth/calendar.readonly` (its CardDAV scope already writes).
    pub fn pim_write_scope(self, access: Access) -> Option<&'static str> {
        match (self, access) {
            (Self::Microsoft, Access::GraphPim) => Some(
                "https://graph.microsoft.com/Contacts.ReadWrite https://graph.microsoft.com/Calendars.ReadWrite",
            ),
            (Self::Google, Access::ImapDav) => Some("https://www.googleapis.com/auth/calendar"),
            _ => None,
        }
    }

    /// Whether a sign-in for `access` without [`Provider::send_scope`] already lets Holtmail
    /// send mail through the account.
    pub fn sends_without_consent(self, access: Access) -> bool {
        self == Self::Google && matches!(access, Access::Imap | Access::ImapDav)
    }
}

/// What a sign-in grants access to. The scopes differ, and a token for one is refused by the
/// other, so an account's kind decides which it signs in for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Access {
    /// Reading the mailbox over IMAP (`AUTHENTICATE XOAUTH2`).
    #[default]
    Imap,
    /// Reading the mailbox through Microsoft Graph; Microsoft only.
    Graph,
    /// Reading the mailbox over IMAP and the contacts and calendars over CardDAV and CalDAV;
    /// Google only. Google has no read-only CardDAV scope, so contacts use the full `carddav`
    /// scope; calendars use `calendar.readonly`, except with Thunderbird's client, which asks
    /// for the `calendar` scope Thunderbird itself asks for.
    ImapDav,
    /// Reading the mailbox, the contacts and the calendars through Microsoft Graph
    /// (`Mail.Read`, `Contacts.Read`, `Calendars.Read`); Microsoft only.
    GraphPim,
}

impl Access {
    /// `imap` or `graph`, as stored and used in the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Imap => "imap",
            Self::Graph => "graph",
            Self::ImapDav => "imap_dav",
            Self::GraphPim => "graph_pim",
        }
    }

    /// Whether a sign-in for `self` also grants everything `other` grants.
    pub fn covers(self, other: Access) -> bool {
        self == other
            || (self == Self::ImapDav && other == Self::Imap)
            || (self == Self::GraphPim && other == Self::Graph)
    }

    /// Parses [`Access::as_str`] output.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "imap" => Some(Self::Imap),
            "graph" => Some(Self::Graph),
            "imap_dav" => Some(Self::ImapDav),
            "graph_pim" => Some(Self::GraphPim),
            _ => None,
        }
    }
}

/// An OAuth client registered with a provider, and where the provider's endpoints are.
#[derive(Clone, PartialEq, Eq)]
pub struct Client {
    /// The provider.
    pub provider: Provider,
    /// The registered client id.
    pub client_id: String,
    /// The client secret; installed-app clients have none or a public one.
    pub client_secret: Option<String>,
    /// Where people sign in.
    pub authorize_url: String,
    /// Where codes and refresh tokens are exchanged.
    pub token_url: String,
    /// Space-separated scopes: mailbox access and a refresh token. [`Client::for_access`]
    /// switches them.
    pub scope: String,
    /// Where the provider sends the browser after sign-in. Must match the registration.
    pub redirect_url: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("provider", &self.provider)
            .field("client_id", &self.client_id)
            .field("redirect_url", &self.redirect_url)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Thunderbird's public installed-app client for `provider`, redirecting to
    /// [`LOOPBACK_REDIRECT`]. The consent screen names Thunderbird, and the provider may block
    /// the client at any time.
    pub fn thunderbird(provider: Provider) -> Self {
        let (client_id, client_secret) = match provider {
            Provider::Google => (GOOGLE_CLIENT_ID, Some(GOOGLE_CLIENT_SECRET)),
            Provider::Microsoft => (MICROSOFT_CLIENT_ID, None),
        };
        Self::own(
            provider,
            client_id,
            client_secret.map(str::to_owned),
            LOOPBACK_REDIRECT,
        )
    }

    /// A client the owner registered, with the provider's endpoints and scopes.
    pub fn own(
        provider: Provider,
        client_id: impl Into<String>,
        client_secret: Option<String>,
        redirect_url: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            client_id: client_id.into(),
            client_secret,
            authorize_url: provider.authorize_endpoint().to_owned(),
            token_url: provider.token_endpoint().to_owned(),
            scope: provider
                .scope(Access::Imap)
                .expect("every provider offers IMAP")
                .to_owned(),
            redirect_url: redirect_url.into(),
        }
    }

    /// The same client asking for the scopes of `access`; `None` when the provider does not
    /// offer it. Codes and refresh tokens are tied to the scopes they were granted for, so a
    /// sign-in and every refresh of its token use the same access.
    pub fn for_access(&self, access: Access) -> Option<Self> {
        let scope = self.provider.scope(access)?;
        let scope = if access == Access::ImapDav && self.is_thunderbird() {
            scope.replace("auth/calendar.readonly", "auth/calendar")
        } else {
            scope.to_owned()
        };
        Some(Self {
            scope,
            ..self.clone()
        })
    }

    /// The same client also asking for [`Provider::send_scope`] of `access`; unchanged when the
    /// provider needs none. A refresh token granted with it is refreshed with it too.
    pub fn with_send(&self, access: Access) -> Self {
        match self.provider.send_scope(access) {
            Some(extra) if !self.scope.split(' ').any(|s| s == extra) => Self {
                scope: format!("{} {extra}", self.scope),
                ..self.clone()
            },
            _ => self.clone(),
        }
    }

    /// The same client also asking for [`Provider::write_scope`] of `access`; unchanged when
    /// the provider needs none. A refresh token granted with it is refreshed with it too.
    pub fn with_write(&self, access: Access) -> Self {
        match self.provider.write_scope(access) {
            Some(extra) if !self.scope.split(' ').any(|s| s == extra) => Self {
                scope: format!("{} {extra}", self.scope),
                ..self.clone()
            },
            _ => self.clone(),
        }
    }

    /// The same client also asking for [`Provider::pim_write_scope`] of `access`; unchanged
    /// when the provider needs none. A refresh token granted with it is refreshed with it too.
    pub fn with_pim_write(&self, access: Access) -> Self {
        let Some(extra) = self.provider.pim_write_scope(access) else {
            return self.clone();
        };
        let mut scopes: Vec<&str> = self.scope.split(' ').collect();
        for wanted in extra.split(' ') {
            if !scopes.contains(&wanted) {
                scopes.push(wanted);
            }
        }
        if scopes.contains(&"https://www.googleapis.com/auth/calendar") {
            scopes.retain(|s| *s != "https://www.googleapis.com/auth/calendar.readonly");
        }
        Self {
            scope: scopes.join(" "),
            ..self.clone()
        }
    }

    /// Whether this is Thunderbird's client rather than one the owner registered.
    pub fn is_thunderbird(&self) -> bool {
        self.client_id == GOOGLE_CLIENT_ID || self.client_id == MICROSOFT_CLIENT_ID
    }

    /// Starts a sign-in: a fresh `state` and PKCE verifier, and the address to open. With
    /// `login_hint` the provider suggests that account. Google is asked for offline access with
    /// a consent prompt, so it always returns a refresh token.
    pub fn authorize(&self, login_hint: Option<&str>) -> Result<Authorization, OAuthError> {
        let state = random_token()?;
        let verifier = random_token()?;
        let url = self.authorize_url_for(&state, &verifier, login_hint);
        Ok(Authorization {
            url,
            state,
            verifier,
        })
    }

    fn authorize_url_for(&self, state: &str, verifier: &str, login_hint: Option<&str>) -> String {
        let mut url = url::Url::parse(&self.authorize_url).expect("a valid authorize endpoint");
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", &self.client_id)
                .append_pair("redirect_uri", &self.redirect_url)
                .append_pair("scope", &self.scope)
                .append_pair("state", state)
                .append_pair("code_challenge", &challenge(verifier))
                .append_pair("code_challenge_method", "S256");
            if let Some(hint) = login_hint.map(str::trim).filter(|h| !h.is_empty()) {
                query.append_pair("login_hint", hint);
            }
            if self.provider == Provider::Google {
                query
                    .append_pair("access_type", "offline")
                    .append_pair("prompt", "consent");
            }
        }
        url.into()
    }

    /// Exchanges the `code` from the redirect, with the verifier of the same sign-in, for tokens.
    /// [`OAuthError::Revoked`] when the code is used, expired or does not match the verifier.
    pub async fn exchange(
        &self,
        endpoint: &dyn TokenEndpoint,
        code: &str,
        verifier: &str,
    ) -> Result<Tokens, OAuthError> {
        let mut form = vec![
            pair("grant_type", "authorization_code"),
            pair("code", code),
            pair("redirect_uri", &self.redirect_url),
            pair("code_verifier", verifier),
        ];
        self.add_client(&mut form);
        self.request(endpoint, form).await
    }

    /// A fresh access token for `refresh_token`. The answer may carry a new refresh token that
    /// replaces the old one. [`OAuthError::Revoked`] when the provider no longer accepts it.
    pub async fn refresh(
        &self,
        endpoint: &dyn TokenEndpoint,
        refresh_token: &str,
    ) -> Result<Tokens, OAuthError> {
        let mut form = vec![
            pair("grant_type", "refresh_token"),
            pair("refresh_token", refresh_token),
        ];
        if self.provider == Provider::Microsoft {
            form.push(pair("scope", &self.scope));
        }
        self.add_client(&mut form);
        self.request(endpoint, form).await
    }

    fn add_client(&self, form: &mut Vec<(String, String)>) {
        form.push(pair("client_id", &self.client_id));
        if let Some(secret) = &self.client_secret {
            form.push(pair("client_secret", secret));
        }
    }

    async fn request(
        &self,
        endpoint: &dyn TokenEndpoint,
        form: Vec<(String, String)>,
    ) -> Result<Tokens, OAuthError> {
        let answer = endpoint.post_form(&self.token_url, &form).await?;
        read_token_answer(&answer)
    }
}

fn pair(key: &str, value: &str) -> (String, String) {
    (key.to_owned(), value.to_owned())
}

/// A sign-in that was started: open `url`, keep `state` and `verifier` until the answer comes.
#[derive(Clone, PartialEq, Eq)]
pub struct Authorization {
    /// The provider's sign-in page with every parameter.
    pub url: String,
    /// The random value the answer must carry back.
    pub state: String,
    /// The PKCE verifier; secret until the code is exchanged.
    pub verifier: String,
}

impl std::fmt::Debug for Authorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authorization").finish_non_exhaustive()
    }
}

/// The PKCE S256 challenge of `verifier`: unpadded base64url of its SHA-256.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_token() -> Result<String, OAuthError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| OAuthError::Random)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Tokens from the provider. `Debug` never shows them.
#[derive(Clone, PartialEq, Eq)]
pub struct Tokens {
    /// Signs in to IMAP until it expires.
    pub access_token: String,
    /// Gets new access tokens; `None` when the provider did not send one.
    pub refresh_token: Option<String>,
    /// How long the access token lives, one hour when the provider does not say.
    pub expires_in: Duration,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("refresh_token", &self.refresh_token.is_some())
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

/// What a token endpoint answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenAnswer {
    /// The HTTP status.
    pub status: u16,
    /// The body as sent.
    pub body: Vec<u8>,
}

/// Posts forms to a provider's token endpoint.
#[async_trait]
pub trait TokenEndpoint: Send + Sync {
    /// Posts `form` as `application/x-www-form-urlencoded` to `url` and returns the status and
    /// body, whatever the status. Only `https` URLs are contacted, except loopback addresses for
    /// tests. [`OAuthError::Connection`] when no answer came; nothing in the error repeats the
    /// form, which holds codes, secrets and tokens.
    async fn post_form(
        &self,
        url: &str,
        form: &[(String, String)],
    ) -> Result<TokenAnswer, OAuthError>;
}

#[derive(Deserialize)]
struct TokenBody {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<serde_json::Value>,
    error: Option<String>,
    error_description: Option<String>,
}

fn read_token_answer(answer: &TokenAnswer) -> Result<Tokens, OAuthError> {
    let body: Option<TokenBody> = serde_json::from_slice(&answer.body).ok();
    if let Some(body) = &body
        && let Some(error) = &body.error
    {
        let reason = body
            .error_description
            .as_deref()
            .map(|d| d.lines().next().unwrap_or_default().trim().to_owned())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| error.clone());
        return Err(match error.as_str() {
            "invalid_grant" => OAuthError::Revoked(reason),
            "temporarily_unavailable" | "server_error" => OAuthError::Protocol(reason),
            _ => OAuthError::Rejected(reason),
        });
    }
    if !(200..300).contains(&answer.status) {
        return Err(OAuthError::Protocol(format!(
            "the token endpoint answered {}",
            answer.status
        )));
    }
    let Some(TokenBody {
        access_token: Some(access_token),
        refresh_token,
        expires_in,
        ..
    }) = body
    else {
        return Err(OAuthError::Protocol(
            "the token endpoint sent no access token".into(),
        ));
    };
    let seconds = match expires_in {
        Some(serde_json::Value::Number(n)) => n.as_u64(),
        Some(serde_json::Value::String(s)) => s.parse().ok(),
        _ => None,
    };
    Ok(Tokens {
        access_token,
        refresh_token: refresh_token.filter(|t| !t.is_empty()),
        expires_in: seconds.map_or(DEFAULT_LIFETIME, Duration::from_secs),
    })
}

/// Why signing in with OAuth failed. Messages never hold tokens, codes or secrets.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OAuthError {
    /// The pasted text is not the address the provider sent the browser to.
    #[error("not a sign-in answer: {0}")]
    Redirect(String),
    /// The person or the provider refused at the sign-in page.
    #[error("access was not granted: {0}")]
    Denied(String),
    /// The code or refresh token is no longer accepted (`invalid_grant`): revoked, expired, the
    /// password changed. Only signing in again helps.
    #[error("the sign-in is no longer valid: {0}")]
    Revoked(String),
    /// The provider refused the request for another reason, e.g. an unknown or blocked client.
    #[error("the provider refused: {0}")]
    Rejected(String),
    /// The token endpoint could not be reached. Retrying later may help.
    #[error("the provider could not be reached: {0}")]
    Connection(String),
    /// The token endpoint answered something unexpected. Retrying later may help.
    #[error("unexpected answer from the provider: {0}")]
    Protocol(String),
    /// The system could not provide random bytes.
    #[error("no random bytes available")]
    Random,
}

impl OAuthError {
    /// Whether retrying the same request later can succeed.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Connection(_) | Self::Protocol(_))
    }
}

#[cfg(test)]
mod tests;
