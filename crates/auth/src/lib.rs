//! Signing in to Holtmail: Argon2id passwords, session tokens, the one-time owner setup link and
//! login rate limiting.
//!
//! [`Auth`] keeps everything durable in a [`UserStore`]; the setup token and the failure counts
//! live in memory and end with the process. Session tokens leave this crate only towards the
//! person signing in: the store holds their BLAKE3 hash.

mod limiter;
mod password;
mod token;

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use holtmail_core::{Timestamp, UserId};
use holtmail_store::{InstanceRole, NewUser, StoreError, User, UserStore};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, OnceCell};

use limiter::Limiter;
pub use password::{
    HashCost, MAX_PASSWORD_BYTES, MIN_PASSWORD_CHARS, Passwords, normalize_email, normalize_name,
    validate_password,
};
pub use token::{random_token, token_hash};

/// Why signing in or managing users failed.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The email is unknown, the user has no password, or the password is wrong. Which one is
    /// deliberately not told.
    #[error("wrong email or password")]
    InvalidCredentials,
    /// Too many failed sign-ins for this address from this client.
    #[error("too many failed sign-ins, try again in {} seconds", .retry_after.as_secs())]
    TooManyAttempts {
        /// When the next attempt is allowed.
        retry_after: Duration,
    },
    /// The setup token is wrong, used, or the instance already has users.
    #[error("the setup link is invalid or was already used")]
    InvalidSetupToken,
    /// Another user has this email.
    #[error("a user with this email exists")]
    EmailTaken,
    /// The input breaks a rule, e.g. a password that is too short.
    #[error("{0}")]
    Invalid(String),
    /// Hashing a password failed.
    #[error("password hashing failed")]
    Hashing,
    /// The system's random number generator failed.
    #[error("no randomness available")]
    Random,
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Tunables of [`Auth`].
#[derive(Debug, Clone)]
pub struct Options {
    /// How long a session lasts from sign-in. Default 30 days.
    pub session_lifetime: Duration,
    /// Failed sign-ins allowed per client address and email within `failure_window`. Default 10.
    pub max_failures: u32,
    /// Default 15 minutes.
    pub failure_window: Duration,
    /// Cost of new password hashes.
    pub hash_cost: HashCost,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            session_lifetime: Duration::from_secs(30 * 24 * 60 * 60),
            max_failures: 10,
            failure_window: Duration::from_secs(15 * 60),
            hash_cost: HashCost::default(),
        }
    }
}

/// A new user's details as entered.
#[derive(Debug, Clone)]
pub struct Signup {
    /// Sign-in address.
    pub email: String,
    /// Name shown to people.
    pub name: String,
    /// Password in clear; never stored or logged.
    pub password: String,
}

/// A session that was just created.
#[derive(Debug, Clone)]
pub struct SignedIn {
    /// Who signed in.
    pub user: User,
    /// The session token to hand to the client, 43 base64url characters. Only its hash is stored.
    pub token: String,
    /// When the session ends.
    pub expires_at: Timestamp,
}

/// Sign-in for one instance.
///
/// Safe to share between tasks. Password hashing runs on Tokio's blocking pool, so the methods
/// must be called inside a Tokio runtime.
pub struct Auth {
    store: Arc<dyn UserStore>,
    options: Options,
    passwords: Passwords,
    setup: Mutex<Option<String>>,
    limiter: Limiter,
    decoy: OnceCell<String>,
}

impl Auth {
    /// Sign-in backed by `store`. Fails with `Invalid` when the hash cost is out of range.
    pub fn new(store: Arc<dyn UserStore>, options: Options) -> Result<Self, AuthError> {
        Ok(Self {
            passwords: Passwords::new(options.hash_cost)?,
            limiter: Limiter::new(options.max_failures, options.failure_window),
            store,
            options,
            setup: Mutex::new(None),
            decoy: OnceCell::new(),
        })
    }

    /// The options in use.
    pub fn options(&self) -> &Options {
        &self.options
    }

    /// Whether the instance has no users yet, so the owner still has to be created.
    pub async fn setup_needed(&self) -> Result<bool, AuthError> {
        Ok(self.store.count_users().await? == 0)
    }

    /// The one-time setup token while the instance has no users, created on the first call and
    /// the same on later calls of this process; `None` once a user exists.
    pub async fn setup_token(&self) -> Result<Option<String>, AuthError> {
        let mut setup = self.setup.lock().await;
        if !self.setup_needed().await? {
            *setup = None;
            return Ok(None);
        }
        if setup.is_none() {
            *setup = Some(random_token()?);
        }
        Ok(setup.clone())
    }

    /// Creates the owner with the setup token and signs them in. The token works once, only
    /// while there are no users, and only after [`Auth::setup_token`] created it; concurrent calls
    /// create at most one owner.
    pub async fn complete_setup(&self, token: &str, signup: Signup) -> Result<SignedIn, AuthError> {
        let mut setup = self.setup.lock().await;
        let matches = setup
            .as_deref()
            .is_some_and(|expected| bool::from(expected.as_bytes().ct_eq(token.as_bytes())));
        if !matches || !self.setup_needed().await? {
            return Err(AuthError::InvalidSetupToken);
        }
        let user = self.create_user(signup, InstanceRole::Owner).await?;
        *setup = None;
        drop(setup);
        self.start_session(user).await
    }

    /// Creates a user with a password. Fails with `Invalid` for a malformed email or name or a
    /// password that breaks [`validate_password`], and with `EmailTaken` for a known email.
    pub async fn create_user(&self, signup: Signup, role: InstanceRole) -> Result<User, AuthError> {
        let email = normalize_email(&signup.email)?;
        let name = normalize_name(&signup.name)?;
        validate_password(&signup.password)?;
        if self.store.find_user_by_email(&email).await?.is_some() {
            return Err(AuthError::EmailTaken);
        }
        let hash = self.hash(signup.password).await?;
        match self
            .store
            .create_user(NewUser {
                email,
                name,
                role,
                password_hash: Some(hash),
            })
            .await
        {
            Err(StoreError::Invalid(_)) => Err(AuthError::EmailTaken),
            other => Ok(other?),
        }
    }

    /// Replaces the user's password and ends all their sessions.
    pub async fn set_password(&self, user: UserId, password: &str) -> Result<(), AuthError> {
        validate_password(password)?;
        let hash = self.hash(password.to_owned()).await?;
        self.store.set_password_hash(user, &hash).await?;
        self.store.delete_user_sessions(user).await?;
        Ok(())
    }

    /// Checks the password and starts a session.
    ///
    /// After `max_failures` failures for the same `client` and email within `failure_window`,
    /// every attempt fails with `TooManyAttempts` without checking the password, until the oldest
    /// failure leaves the window. A success clears the failures. Unknown emails take as long as
    /// wrong passwords.
    pub async fn login(
        &self,
        client: IpAddr,
        email: &str,
        password: &str,
    ) -> Result<SignedIn, AuthError> {
        let key = email.trim().to_lowercase();
        if let Some(retry_after) = self.limiter.blocked(client, &key, Instant::now()) {
            return Err(AuthError::TooManyAttempts { retry_after });
        }
        let user = self.store.find_user_by_email(email.trim()).await?;
        let hash = match &user {
            Some(user) => self.store.password_hash(user.id).await?,
            None => None,
        };
        let valid = match hash {
            Some(hash) => self.verify(password.to_owned(), hash).await?,
            None => {
                let decoy = self.decoy().await?;
                self.verify(password.to_owned(), decoy).await?;
                false
            }
        };
        match user {
            Some(user) if valid => {
                self.limiter.clear(client, &key);
                self.start_session(user).await
            }
            _ => {
                self.limiter.fail(client, &key, Instant::now());
                Err(AuthError::InvalidCredentials)
            }
        }
    }

    /// The user of a live session, or `None` for an unknown, malformed or expired token.
    pub async fn session(&self, token: &str) -> Result<Option<User>, AuthError> {
        match token_hash(token) {
            Some(hash) => Ok(self.store.session_user(&hash, Timestamp::now()).await?),
            None => Ok(None),
        }
    }

    /// Ends the session. Idempotent; malformed tokens are ignored.
    pub async fn logout(&self, token: &str) -> Result<(), AuthError> {
        if let Some(hash) = token_hash(token) {
            self.store.delete_session(&hash).await?;
        }
        Ok(())
    }

    /// Forgets expired sessions and old failure counts; returns how many sessions were removed.
    pub async fn prune(&self) -> Result<u64, AuthError> {
        self.limiter.prune(Instant::now());
        Ok(self.store.delete_expired_sessions(Timestamp::now()).await?)
    }

    async fn start_session(&self, user: User) -> Result<SignedIn, AuthError> {
        let token = random_token()?;
        let hash = token_hash(&token).ok_or(AuthError::Random)?;
        let lifetime = i64::try_from(self.options.session_lifetime.as_millis()).unwrap_or(i64::MAX);
        let expires_at =
            Timestamp::from_millis(Timestamp::now().as_millis().saturating_add(lifetime));
        self.store
            .create_session(&hash, user.id, expires_at)
            .await?;
        Ok(SignedIn {
            user,
            token,
            expires_at,
        })
    }

    async fn hash(&self, password: String) -> Result<String, AuthError> {
        let passwords = self.passwords.clone();
        tokio::task::spawn_blocking(move || passwords.hash(&password))
            .await
            .map_err(|_| AuthError::Hashing)?
    }

    async fn verify(&self, password: String, hash: String) -> Result<bool, AuthError> {
        let passwords = self.passwords.clone();
        tokio::task::spawn_blocking(move || passwords.verify(&password, &hash))
            .await
            .map_err(|_| AuthError::Hashing)
    }

    async fn decoy(&self) -> Result<String, AuthError> {
        self.decoy
            .get_or_try_init(|| async { self.hash(random_token()?).await })
            .await
            .cloned()
    }
}

#[cfg(test)]
mod tests;
