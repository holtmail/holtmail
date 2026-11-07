//! Link helpers shared by insights and chats: unwrapping of known redirect wrappers, removal of
//! tracking parameters, detection of opaque ESP click trackers, registrable domains on the Public
//! Suffix List and the IDN display policy.
//!
//! Every function is pure: no I/O, no network, no logging. All of them are deterministic for a
//! given [`version`] and [`psl_version`], accept any input and never panic.

#![deny(clippy::string_slice, clippy::indexing_slicing)]

mod data;
mod display;
mod domain;
mod percent;
mod tracking;
mod unwrap;

pub use data::{psl_version, version};
pub use display::{DisplayHost, display_host};
pub use domain::registrable_domain;
pub use tracking::{strip_tracking, tracker};
pub use unwrap::{MAX_DEPTH, MAX_INPUT_BYTES, Unwrapped, unwrap};

/// The version of the link rules: the unwrap allow-list and decoders, tracking-parameter
/// matching and tracker matching. It is bumped by hand on any change of code behaviour; changes of
/// the bundled data files show up in [`version`], which embeds it.
pub const VERSION: &str = "1";
