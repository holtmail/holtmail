//! Stalwart: users created through its management API, and a JMAP seeder for mail, contacts and
//! calendars.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::jmap::{Auth, Jmap};

/// HTTPS (JMAP, CardDAV, CalDAV) on this machine, with a certificate from the stack's CA for
/// `localhost`. The session names `https://localhost:43443` as its public address.
pub const STALWART_HTTPS: u16 = 43443;
/// Plain IMAP with STARTTLS; `LOGIN` is allowed without TLS.
pub const STALWART_IMAP: u16 = 43143;
/// IMAP over TLS.
pub const STALWART_IMAPS: u16 = 43993;
/// The password of every user [`create_user`] makes.
pub const STALWART_PASSWORD: &str = "it-stalwart-password";
/// The recovery administrator from `compose.yaml`, which manages users while the server runs.
pub const STALWART_ADMIN: (&str, &str) = ("admin", "it-stalwart-admin-password");

/// `using` for mail.
pub const MAIL: &[&str] = &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"];
/// `using` for contacts.
pub const CONTACTS: &[&str] = &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:contacts"];
/// `using` for calendars.
pub const CALENDARS: &[&str] = &[
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:calendars",
];
const MANAGE: &[&str] = &["urn:ietf:params:jmap:core", "urn:stalwart:jmap"];

/// The server's address, where Holtmail asks `/.well-known/jmap`.
pub fn base_url() -> String {
    format!("https://localhost:{STALWART_HTTPS}")
}

/// The session address.
pub fn session_url() -> String {
    format!("{}/jmap/session", base_url())
}

async fn admin() -> Jmap {
    Jmap::connect(&session_url(), STALWART_ADMIN.0, STALWART_ADMIN.1).await
}

/// Creates `email` (on `example.com`) with [`STALWART_PASSWORD`]; nothing happens when it
/// exists already.
pub async fn create_user(email: &str) {
    let (name, domain) = email.split_once('@').expect("an address");
    assert_eq!(
        domain, "example.com",
        "the stack's Stalwart serves example.com"
    );
    let admin = admin().await;
    let domains = admin
        .ok(
            MANAGE,
            "x:Domain/query",
            json!({ "filter": { "name": domain } }),
        )
        .await;
    let domain_id = domains["ids"][0]
        .as_str()
        .unwrap_or_else(|| panic!("no domain {domain}: {domains}"))
        .to_owned();
    let answers = admin
        .call(
            MANAGE,
            json!([["x:Account/set", { "create": { "user": {
                "@type": "User",
                "name": name,
                "domainId": domain_id,
                "credentials": { "0": { "@type": "Password", "secret": STALWART_PASSWORD } },
                "roles": { "@type": "User" },
                "permissions": { "@type": "Inherit" },
                "encryptionAtRest": { "@type": "Disabled" },
            } } }, "0"]]),
        )
        .await;
    let answer = &answers[0][1];
    if let Some(refused) = answer["notCreated"]["user"].as_object() {
        let text = Value::Object(refused.clone()).to_string();
        assert!(
            text.contains("exist") || text.contains("primaryKey"),
            "Stalwart refused {email}: {text}"
        );
    }
}

/// A new API key of `email`, sent as a bearer token.
pub async fn api_key(email: &str) -> String {
    let jmap = Jmap::connect(&session_url(), email, STALWART_PASSWORD).await;
    let created = jmap
        .ok(
            MANAGE,
            "x:ApiKey/set",
            json!({ "create": { "key": {
                "description": "holtmail integration test",
                "permissions": { "@type": "Inherit" },
                "allowedIps": {},
            } } }),
        )
        .await;
    created["created"]["key"]["secret"]
        .as_str()
        .unwrap_or_else(|| panic!("no secret in {created}"))
        .to_owned()
}

/// A JMAP client of one Stalwart user, for seeding.
pub struct Seeder {
    /// The signed-in client.
    pub jmap: Jmap,
    mail: String,
}

impl Seeder {
    /// Creates `email` when needed and signs it in with its password.
    pub async fn new(email: &str) -> Self {
        create_user(email).await;
        Self::with(Auth::Basic(email.to_owned(), STALWART_PASSWORD.to_owned())).await
    }

    /// Signs in with `auth`.
    pub async fn with(auth: Auth) -> Self {
        let jmap = Jmap::connect_with(&session_url(), auth).await;
        let mail = jmap.account("urn:ietf:params:jmap:mail");
        Self { jmap, mail }
    }

    /// Mailbox ids by path (names joined with `/`), and their roles.
    pub async fn mailboxes(&self) -> BTreeMap<String, (String, Option<String>)> {
        let answer = self
            .jmap
            .ok(
                MAIL,
                "Mailbox/get",
                json!({ "accountId": self.mail, "properties": ["name", "parentId", "role"] }),
            )
            .await;
        let list = answer["list"].as_array().cloned().unwrap_or_default();
        let by_id: BTreeMap<String, Value> = list
            .iter()
            .map(|m| (m["id"].as_str().unwrap_or_default().to_owned(), m.clone()))
            .collect();
        let path = |mut id: String| {
            let mut names = Vec::new();
            while let Some(mailbox) = by_id.get(&id) {
                names.push(mailbox["name"].as_str().unwrap_or_default().to_owned());
                match mailbox["parentId"].as_str() {
                    Some(parent) => id = parent.to_owned(),
                    None => break,
                }
            }
            names.reverse();
            names.join("/")
        };
        list.iter()
            .map(|m| {
                let id = m["id"].as_str().unwrap_or_default().to_owned();
                (
                    path(id.clone()),
                    (id, m["role"].as_str().map(str::to_owned)),
                )
            })
            .collect()
    }

    /// The id of the mailbox with `role`, such as `inbox`.
    pub async fn role(&self, role: &str) -> String {
        self.mailboxes()
            .await
            .into_values()
            .find(|(_, r)| r.as_deref() == Some(role))
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("no mailbox with the role {role}"))
    }

    /// Creates a mailbox named `name` under `parent` and returns its id.
    pub async fn create_mailbox(&self, name: &str, parent: Option<&str>) -> String {
        let created = self
            .jmap
            .ok(
                MAIL,
                "Mailbox/set",
                json!({ "accountId": self.mail, "create": { "m": { "name": name, "parentId": parent } } }),
            )
            .await;
        created["created"]["m"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("no mailbox id in {created}"))
            .to_owned()
    }

    /// Destroys the mailbox `id` with the emails only in it.
    pub async fn destroy_mailbox(&self, id: &str) {
        self.jmap
            .ok(
                MAIL,
                "Mailbox/set",
                json!({ "accountId": self.mail, "destroy": [id], "onDestroyRemoveEmails": true }),
            )
            .await;
    }

    /// Imports `bytes` into `mailboxes` with `keywords` (such as `$seen`) and returns the email id.
    pub async fn import(&self, bytes: &[u8], mailboxes: &[&str], keywords: &[&str]) -> String {
        let blob = self.jmap.upload(bytes).await;
        let mailbox_ids: serde_json::Map<String, Value> = mailboxes
            .iter()
            .map(|m| ((*m).to_owned(), Value::Bool(true)))
            .collect();
        let keywords: serde_json::Map<String, Value> = keywords
            .iter()
            .map(|k| ((*k).to_owned(), Value::Bool(true)))
            .collect();
        let imported = self
            .jmap
            .ok(
                MAIL,
                "Email/import",
                json!({ "accountId": self.mail, "emails": { "e": {
                    "blobId": blob,
                    "mailboxIds": mailbox_ids,
                    "keywords": keywords,
                } } }),
            )
            .await;
        imported["created"]["e"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("no email id in {imported}"))
            .to_owned()
    }

    /// Applies `patch` (such as `{"keywords/$seen": true}`) to the email `id`.
    pub async fn update(&self, id: &str, patch: Value) {
        self.jmap
            .ok(
                MAIL,
                "Email/set",
                json!({ "accountId": self.mail, "update": { id: patch } }),
            )
            .await;
    }

    /// Destroys the email `id`.
    pub async fn destroy(&self, id: &str) {
        self.jmap
            .ok(
                MAIL,
                "Email/set",
                json!({ "accountId": self.mail, "destroy": [id] }),
            )
            .await;
    }
}
