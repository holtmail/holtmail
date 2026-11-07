//! Shared tests for [`Categorizer`](crate::Categorizer) implementations, and a fake.
//!
//! Every categorizer runs [`conformance::categorizer`] in its own tests with its own cases.
//! [`FakeCategorizer`] stands in for a table-backed categorizer in tests of the runner and the
//! server.

pub mod conformance;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::{Categorizer, CategoryInput, CategorySnapshot, CategoryVote};

/// A categorizer that answers from a map of sender domains to votes.
///
/// [`FakeCategorizer::swap`] replaces the version and the map at once, like a table reload:
/// snapshots taken before it keep the old version and answers.
#[derive(Debug)]
pub struct FakeCategorizer {
    id: String,
    current: Mutex<Arc<FakeSnapshot>>,
}

#[derive(Debug)]
struct FakeSnapshot {
    version: String,
    votes: HashMap<String, CategoryVote>,
}

impl FakeCategorizer {
    /// A categorizer with this id and version that answers `votes[sender_domain]`, and `None`
    /// for any other domain or no domain.
    pub fn new(
        id: impl Into<String>,
        version: impl Into<String>,
        votes: HashMap<String, CategoryVote>,
    ) -> Self {
        Self {
            id: id.into(),
            current: Mutex::new(Arc::new(FakeSnapshot {
                version: version.into(),
                votes,
            })),
        }
    }

    /// Replaces the version and the map. Later snapshots see the new ones; earlier snapshots keep
    /// theirs.
    pub fn swap(&self, version: impl Into<String>, votes: HashMap<String, CategoryVote>) {
        let next = Arc::new(FakeSnapshot {
            version: version.into(),
            votes,
        });
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = next;
    }
}

impl Categorizer for FakeCategorizer {
    fn id(&self) -> &str {
        &self.id
    }

    fn snapshot(&self) -> Arc<dyn CategorySnapshot> {
        self.current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl CategorySnapshot for FakeSnapshot {
    fn version(&self) -> String {
        self.version.clone()
    }

    fn categorize(&self, input: &CategoryInput) -> Option<CategoryVote> {
        self.votes.get(input.sender_domain.as_deref()?).cloned()
    }
}
