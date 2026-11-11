//! The in-memory fake passes the conformance suite.

use holtmail_mailview::testing::{FakeHarness, conformance};

#[tokio::test]
async fn the_fake_keeps_the_contract() {
    conformance(&FakeHarness).await;
}
