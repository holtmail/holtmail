//! Helpers for the integration tests: run the real `holtmail` binary against the servers in
//! `compose.yaml`, drive it through its HTTP API, seed the servers with invented data and check
//! the logs for leaks.
//!
//! Every test starts with [`require_it!`], so `cargo test --workspace` stays network-free: the
//! tests only run with `HOLTMAIL_IT=1`, which `mise run it` sets after starting the stack.

pub mod api;
pub mod categories;
pub mod data;
pub mod dav;
pub mod dkim;
pub mod docker;
pub mod holtmail;
pub mod images;
pub mod imap;
pub mod jmap;
pub mod logs;
pub mod memory;
pub mod oauth;
pub mod pim;
pub mod proxy;
pub mod redirect;
pub mod relay;
pub mod s3;
pub mod services;
pub mod stalwart;
pub mod submit;
pub mod unsubscribe;

pub use api::Api;
pub use holtmail::{Holtmail, Options};
pub use logs::LogScanner;

use std::path::PathBuf;

/// Whether integration tests run: `HOLTMAIL_IT` is `1`.
pub fn enabled() -> bool {
    std::env::var("HOLTMAIL_IT").is_ok_and(|v| v == "1")
}

/// Returns from the calling test unless [`enabled`].
#[macro_export]
macro_rules! require_it {
    () => {
        if !$crate::enabled() {
            eprintln!("skipped: set HOLTMAIL_IT=1 or run `mise run it`");
            return;
        }
    };
}

/// Whether the optional compose profile `name` is on: `IT_PROFILES` names it.
pub fn profile(name: &str) -> bool {
    std::env::var("IT_PROFILES").is_ok_and(|p| p.split(',').any(|p| p.trim() == name))
}

/// The root of the repository.
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root exists")
}

/// `tests/integration/.state`, where the stack's CA and server certificates are written.
pub fn state_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".state")
}

/// The PEM file of the stack's throwaway CA, which signs every server certificate.
pub fn ca_file() -> PathBuf {
    state_dir().join("ca.crt")
}

/// A short token unique within this machine and run, for user names, folders and subjects.
pub fn unique(prefix: &str) -> String {
    let id = uuid::Uuid::now_v7().simple().to_string();
    format!("{prefix}-{}", &id[id.len() - 12..])
}
