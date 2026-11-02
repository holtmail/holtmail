//! Conformance suite every [`BlobStore`] implementation runs in its own tests.

use std::sync::Arc;

use bytes::Bytes;
use tokio::task::JoinSet;

use crate::{BlobId, BlobStore};

/// Checks that `store` keeps the [`BlobStore`] contract. Panics on the first violation.
///
/// `store` must be empty when the suite starts. Call it from a multi-threaded Tokio runtime
/// (`#[tokio::test(flavor = "multi_thread")]`) so concurrent writes really race.
pub async fn conformance(store: Arc<dyn BlobStore>) {
    let message = Bytes::from_static(
        b"From: Ada <ada@example.com>\r\nTo: Bob <bob@example.org>\r\nSubject: Lunch\r\n\r\nNoon?\r\n",
    );
    let id = BlobId::of(&message);

    assert!(
        !store.contains(&id).await.unwrap(),
        "empty store contains a blob"
    );
    assert!(
        store.get(&id).await.unwrap().is_none(),
        "empty store returned a blob"
    );

    assert_eq!(
        store.put(message.clone()).await.unwrap(),
        id,
        "put returned a wrong id"
    );
    assert!(store.contains(&id).await.unwrap(), "stored blob is missing");
    assert_eq!(
        store.get(&id).await.unwrap(),
        Some(message.clone()),
        "stored bytes changed"
    );

    assert_eq!(
        store.put(message.clone()).await.unwrap(),
        id,
        "second put is not idempotent"
    );

    let empty = Bytes::new();
    let empty_id = store.put(empty.clone()).await.unwrap();
    assert_eq!(
        store.get(&empty_id).await.unwrap(),
        Some(empty),
        "empty blob changed"
    );

    store.delete(&id).await.unwrap();
    let mut racers = JoinSet::new();
    for _ in 0..8 {
        let store = store.clone();
        let message = message.clone();
        racers.spawn(async move { store.put(message).await });
    }
    while let Some(result) = racers.join_next().await {
        assert_eq!(result.unwrap().unwrap(), id, "concurrent put failed");
    }
    assert_eq!(
        store.get(&id).await.unwrap(),
        Some(message),
        "concurrent put corrupted the blob"
    );

    store.delete(&id).await.unwrap();
    assert!(
        !store.contains(&id).await.unwrap(),
        "deleted blob is still there"
    );
    assert!(
        store.get(&id).await.unwrap().is_none(),
        "deleted blob is still readable"
    );
    store.delete(&id).await.unwrap();
}
