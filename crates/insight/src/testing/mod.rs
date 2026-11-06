//! Shared tests for [`InsightExtractor`](crate::InsightExtractor) implementations.
//!
//! Every extractor runs [`conformance::extractor`] in its own tests with its own fixture cases.
//! The suite adds plain personal mail ([`personal_mail`]) and the shared hostile corpus
//! ([`hostile_corpus`]) to those cases.

pub mod conformance;
mod corpus;
mod zip;

pub use corpus::{hostile_corpus, personal_mail};

use crate::InsightInput;

/// One input for the conformance suite, with what the extractor must make of it.
#[derive(Debug, Clone)]
pub struct Case {
    /// A name for failure messages.
    pub name: String,
    /// The input.
    pub input: InsightInput,
    /// What the output must be.
    pub expect: Expect,
}

impl Case {
    /// A case with this name, input and expectation.
    pub fn new(name: impl Into<String>, input: InsightInput, expect: Expect) -> Self {
        Self {
            name: name.into(),
            input,
            expect,
        }
    }
}

/// What a [`Case`] requires of the output, on top of the suite's general rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// No items.
    Nothing,
    /// At least one item.
    Found,
    /// Anything that keeps the general rules.
    Any,
}
