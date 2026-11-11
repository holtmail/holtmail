use std::sync::Arc;

use holtmail_core::{ArchiveId, FlowId, IdentityId, Timestamp, UserId};
use serde_json::json;

use super::changes::{message, user};
use crate::*;

pub(super) async fn run(store: Arc<dyn Store>) {
    let archive = store.create_archive("Rules").await.unwrap().id;
    let ada = user(&*store, "rules-ada@example.com").await;
    flows_and_versions(&*store, archive, ada).await;
    order_and_modes(&*store, archive, ada).await;
    runs_once_per_version_and_event(store.clone(), archive, ada).await;
    actions_and_counters(&*store, archive, ada).await;
    events_wait_in_order(&*store, archive).await;
    deleting(&*store, archive, ada).await;
}

fn t(secs: i64) -> Timestamp {
    Timestamp::from_secs(1_800_000_000 + secs)
}

async fn flow(store: &dyn Store, archive: ArchiveId, user: UserId, name: &str) -> FlowRecord {
    store
        .create_flow(&NewFlow {
            archive_id: archive,
            name: name.into(),
            definition: format!("{{\"name\":\"{name}\"}}"),
            trigger: "message.archived".into(),
            saved_by: user,
            at: t(0),
        })
        .await
        .unwrap()
}

fn new_run(f: &FlowRecord, event: &str, started: Timestamp) -> NewFlowRun {
    NewFlowRun {
        flow_id: f.id,
        archive_id: f.archive_id,
        version: f.version,
        event_id: event.into(),
        trigger: "message.archived".into(),
        test: false,
        message_id: None,
        sender: Some("bob@example.org".into()),
        started_at: started,
    }
}

async fn flows_and_versions(store: &dyn Store, archive: ArchiveId, ada: UserId) {
    let missing = store
        .create_flow(&NewFlow {
            archive_id: ArchiveId::new(),
            name: "x".into(),
            definition: "{}".into(),
            trigger: "manual".into(),
            saved_by: ada,
            at: t(0),
        })
        .await;
    assert!(matches!(missing, Err(StoreError::NotFound(_))));

    let f = flow(store, archive, ada, "versions").await;
    assert_eq!(f.mode, FlowMode::Test, "a new flow starts in test mode");
    assert_eq!(f.version, 1);
    assert_eq!(f.paused, None);
    assert_eq!(store.get_flow(f.id).await.unwrap(), Some(f.clone()));
    assert_eq!(store.get_flow(FlowId::new()).await.unwrap(), None);

    let saved = store
        .save_flow(f.id, 1, "{\"v\":2}", "schedule", ada, t(10))
        .await
        .unwrap();
    let FlowSave::Saved(saved) = saved else {
        panic!("the current version saves");
    };
    assert_eq!(saved.version, 2);
    assert_eq!(saved.trigger, "schedule");
    assert_eq!(saved.mode, FlowMode::Test, "saving keeps the mode");
    assert_eq!(
        store
            .save_flow(f.id, 1, "{\"v\":3}", "schedule", ada, t(20))
            .await
            .unwrap(),
        FlowSave::Stale { current: 2 },
        "saving over an older version is refused"
    );
    let versions = store.flow_versions(f.id).await.unwrap();
    assert_eq!(
        versions.iter().map(|v| v.version).collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert_eq!(
        store
            .flow_version(f.id, 1)
            .await
            .unwrap()
            .unwrap()
            .definition,
        "{\"name\":\"versions\"}"
    );
    assert_eq!(store.flow_version(f.id, 9).await.unwrap(), None);
    assert!(matches!(
        store
            .save_flow(FlowId::new(), 1, "{}", "manual", ada, t(0))
            .await,
        Err(StoreError::NotFound(_))
    ));

    store
        .change_flow(
            f.id,
            &FlowChange {
                mode: Some(FlowMode::On),
                ..FlowChange::default()
            },
            t(30),
        )
        .await
        .unwrap();
    assert!(
        store
            .pause_flow(f.id, "circuit_breaker", t(40))
            .await
            .unwrap()
    );
    assert!(
        !store
            .pause_flow(f.id, "circuit_breaker", t(41))
            .await
            .unwrap(),
        "only the first pause counts"
    );
    let paused = store.get_flow(f.id).await.unwrap().unwrap();
    assert_eq!(
        paused.paused,
        Some(FlowPause {
            reason: "circuit_breaker".into(),
            at: t(40)
        })
    );
    assert_eq!(paused.mode, FlowMode::On);
    let resumed = store
        .change_flow(
            f.id,
            &FlowChange {
                mode: Some(FlowMode::On),
                ..FlowChange::default()
            },
            t(50),
        )
        .await
        .unwrap();
    assert_eq!(resumed.paused, None, "setting a mode clears the pause");
    assert!(store.delete_flow(f.id).await.unwrap());
}

async fn order_and_modes(store: &dyn Store, archive: ArchiveId, ada: UserId) {
    let a = flow(store, archive, ada, "a").await;
    let b = flow(store, archive, ada, "b").await;
    let c = flow(store, archive, ada, "c").await;
    let names = |flows: Vec<FlowRecord>| {
        flows
            .into_iter()
            .map(|f| (f.name, f.position))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(store.list_flows(archive).await.unwrap()),
        vec![("a".into(), 0), ("b".into(), 1), ("c".into(), 2)]
    );
    let moved = store
        .change_flow(
            c.id,
            &FlowChange {
                name: Some("c2".into()),
                position: Some(0),
                ..FlowChange::default()
            },
            t(1),
        )
        .await
        .unwrap();
    assert_eq!(moved.name, "c2");
    assert_eq!(
        names(store.list_flows(archive).await.unwrap()),
        vec![("c2".into(), 0), ("a".into(), 1), ("b".into(), 2)]
    );
    store
        .change_flow(
            c.id,
            &FlowChange {
                position: Some(99),
                ..FlowChange::default()
            },
            t(2),
        )
        .await
        .unwrap();
    assert!(store.delete_flow(a.id).await.unwrap());
    assert!(!store.delete_flow(a.id).await.unwrap());
    assert_eq!(
        names(store.list_flows(archive).await.unwrap()),
        vec![("b".into(), 0), ("c2".into(), 1)],
        "positions stay without gaps"
    );
    assert!(matches!(
        store
            .change_flow(FlowId::new(), &FlowChange::default(), t(0))
            .await,
        Err(StoreError::NotFound(_))
    ));
    store.delete_flow(b.id).await.unwrap();
    store.delete_flow(c.id).await.unwrap();
}

async fn runs_once_per_version_and_event(store: Arc<dyn Store>, archive: ArchiveId, ada: UserId) {
    let f = flow(&*store, archive, ada, "runs").await;
    let first = store
        .claim_flow_run(&new_run(&f, "archived:1", t(0)))
        .await
        .unwrap()
        .expect("a new run is claimed");
    assert_eq!(first.status, FlowRunStatus::Running);
    assert_eq!(first.results, json!([]));
    assert!(
        store
            .claim_flow_run(&new_run(&f, "archived:1", t(1)))
            .await
            .unwrap()
            .is_none(),
        "the same event runs once per version"
    );
    let mut claims = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let run = new_run(&f, "archived:race", t(2));
        claims.push(tokio::spawn(async move {
            store.claim_flow_run(&run).await.unwrap().is_some()
        }));
    }
    let mut won = 0;
    for claim in claims {
        won += usize::from(claim.await.unwrap());
    }
    assert_eq!(won, 1, "concurrent claims of one run: exactly one wins");

    let FlowSave::Saved(v2) = store
        .save_flow(f.id, 1, "{}", "message.archived", ada, t(3))
        .await
        .unwrap()
    else {
        panic!("saves");
    };
    assert!(
        store
            .claim_flow_run(&new_run(&v2, "archived:1", t(4)))
            .await
            .unwrap()
            .is_some(),
        "a new version runs the event again"
    );

    store
        .finish_flow_run(
            first.id,
            &FlowRunEnd {
                status: FlowRunStatus::Done,
                plan: Some(json!({"ran": true})),
                results: json!([{"result": "done"}]),
                error: None,
                acted: true,
                at: t(5),
            },
        )
        .await
        .unwrap();
    let done = store.get_flow_run(first.id).await.unwrap().unwrap();
    assert_eq!(done.status, FlowRunStatus::Done);
    assert_eq!(done.plan, Some(json!({"ran": true})));
    assert!(done.acted);
    assert_eq!(done.finished_at, Some(t(5)));
    assert!(
        !store
            .set_flow_run_result(first.id, 0, "pending", &json!({"result": "confirmed"}))
            .await
            .unwrap(),
        "only a result holding the expected value changes"
    );
    assert!(
        !store
            .set_flow_run_result(first.id, 3, "done", &json!({"result": "confirmed"}))
            .await
            .unwrap(),
        "no such index"
    );
    store
        .finish_flow_run(
            first.id,
            &FlowRunEnd {
                status: FlowRunStatus::Done,
                plan: Some(json!({"ran": true})),
                results: json!([{"result": "pending"}, {"result": "pending"}]),
                error: None,
                acted: true,
                at: t(5),
            },
        )
        .await
        .unwrap();
    let mut decisions = Vec::new();
    for i in 0..8 {
        let store = store.clone();
        let id = first.id;
        decisions.push(tokio::spawn(async move {
            store
                .set_flow_run_result(id, i % 2, "pending", &json!({"result": format!("by {i}")}))
                .await
                .unwrap()
        }));
    }
    let mut won = 0;
    for decision in decisions {
        won += usize::from(decision.await.unwrap());
    }
    assert_eq!(won, 2, "one decision per held action wins");
    let results = store.get_flow_run(first.id).await.unwrap().unwrap().results;
    assert!(
        results
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["result"].as_str().unwrap().starts_with("by ")),
        "decisions on different actions keep each other: {results}"
    );
    assert_eq!(
        store
            .flow_run_for_event(f.id, "archived:1")
            .await
            .unwrap()
            .unwrap()
            .new
            .version,
        2,
        "the newest run of the event, of any version"
    );
    assert!(
        store
            .flow_run_for_event(f.id, "archived:none")
            .await
            .unwrap()
            .is_none()
    );

    let page = store.flow_runs(f.id, None, 2).await.unwrap();
    assert_eq!(page.len(), 2);
    assert!(page[0].id > page[1].id, "newest first");
    let rest = store.flow_runs(f.id, Some(page[1].id), 10).await.unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].id, first.id);

    assert_eq!(store.interrupt_flow_runs(t(6)).await.unwrap(), 2);
    assert!(
        store
            .flow_runs(f.id, None, 10)
            .await
            .unwrap()
            .iter()
            .all(|r| r.status != FlowRunStatus::Running)
    );
    assert_eq!(
        store
            .count_flow_runs(f.id, t(0), false, None)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        store.count_flow_runs(f.id, t(0), true, None).await.unwrap(),
        1
    );
    assert_eq!(
        store
            .count_flow_runs(f.id, t(0), false, Some("BOB@example.org"))
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        store
            .count_flow_runs(f.id, t(3), false, None)
            .await
            .unwrap(),
        1
    );
    assert_eq!(store.prune_flow_runs(t(3), t(0)).await.unwrap(), 2);
    assert_eq!(store.flow_runs(f.id, None, 10).await.unwrap().len(), 1);
    store.delete_flow(f.id).await.unwrap();
}

async fn actions_and_counters(store: &dyn Store, archive: ArchiveId, ada: UserId) {
    let f = flow(store, archive, ada, "actions").await;
    let run = store
        .claim_flow_run(&new_run(&f, "archived:a", t(0)))
        .await
        .unwrap()
        .unwrap();
    let identity = IdentityId::new();
    for (at, kind, recipient) in [
        (t(10), "reply", Some("Bob@Example.org")),
        (t(20), "reply", Some("bob@example.org")),
        (t(30), "move", None),
    ] {
        store
            .record_flow_action(&FlowAction {
                run_id: run.id,
                flow_id: f.id,
                kind: kind.into(),
                identity: recipient.map(|_| identity),
                recipient: recipient.map(Into::into),
                at,
            })
            .await
            .unwrap();
    }
    assert_eq!(store.count_flow_actions(f.id, t(15)).await.unwrap(), 2);
    assert_eq!(
        store
            .count_identity_flow_actions(identity, t(0))
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .last_flow_reply(f.id, "BOB@example.org")
            .await
            .unwrap(),
        Some(t(20))
    );
    assert_eq!(
        store
            .last_flow_reply(f.id, "carol@example.org")
            .await
            .unwrap(),
        None
    );
    store.prune_flow_runs(t(0), t(25)).await.unwrap();
    assert_eq!(store.count_flow_actions(f.id, t(0)).await.unwrap(), 1);

    let first = message(store, archive, 9001).await;
    message(store, archive, 9002).await;
    assert_eq!(
        store
            .count_messages_from(archive, "ADA@example.com", Timestamp::from_secs(0), None)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .count_messages_from(
                archive,
                "ada@example.com",
                Timestamp::from_secs(0),
                Some(first)
            )
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .count_messages_from(archive, "bob@example.org", Timestamp::from_secs(0), None)
            .await
            .unwrap(),
        0,
        "only From counts"
    );
    store.delete_flow(f.id).await.unwrap();
}

async fn events_wait_in_order(store: &dyn Store, archive: ArchiveId) {
    let event = |id: &str, at: i64| RuleEvent {
        id: id.into(),
        archive_id: archive,
        trigger: "message.archived".into(),
        message_id: None,
        item_id: None,
        live: true,
        payload: json!({"n": at}),
        at: t(at),
    };
    assert!(store.put_rule_event(&event("b", 2)).await.unwrap());
    assert!(store.put_rule_event(&event("a", 1)).await.unwrap());
    assert!(
        !store.put_rule_event(&event("b", 3)).await.unwrap(),
        "a waiting event is kept once"
    );
    let due = store.due_rule_events(10).await.unwrap();
    assert_eq!(
        due.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        vec!["b", "a"],
        "in the order they were kept"
    );
    assert_eq!(due[0].payload, json!({"n": 2}));
    assert_eq!(store.due_rule_events(1).await.unwrap().len(), 1);
    store.drop_rule_event(archive, "b").await.unwrap();
    store.drop_rule_event(archive, "b").await.unwrap();
    store.drop_rule_event(archive, "a").await.unwrap();
    assert!(store.due_rule_events(10).await.unwrap().is_empty());
}

async fn deleting(store: &dyn Store, archive: ArchiveId, ada: UserId) {
    let f = flow(store, archive, ada, "gone").await;
    let run = store
        .claim_flow_run(&new_run(&f, "archived:gone", t(0)))
        .await
        .unwrap()
        .unwrap();
    assert!(store.delete_flow(f.id).await.unwrap());
    assert_eq!(store.get_flow_run(run.id).await.unwrap(), None);
    assert!(store.flow_versions(f.id).await.unwrap().is_empty());

    let doomed = store.create_archive("Doomed rules").await.unwrap().id;
    let g = flow(store, doomed, ada, "doomed").await;
    store
        .put_rule_event(&RuleEvent {
            id: "x".into(),
            archive_id: doomed,
            trigger: "manual".into(),
            message_id: None,
            item_id: None,
            live: false,
            payload: json!({}),
            at: t(0),
        })
        .await
        .unwrap();
    store.delete_archive(doomed).await.unwrap();
    assert_eq!(store.get_flow(g.id).await.unwrap(), None);
    assert!(store.due_rule_events(10).await.unwrap().is_empty());
}
