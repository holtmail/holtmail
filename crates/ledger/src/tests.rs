use super::*;

fn chain(n: u64) -> Vec<LedgerEntry> {
    let archive = ArchiveId::new();
    let mut prev = LedgerHash::ZERO;
    (1..=n)
        .map(|seq| {
            let entry = NewEntry {
                at: Timestamp::from_millis(1_741_600_000_000 + seq as i64),
                event: LedgerEvent::Archived,
                archive_id: archive,
                message_id: Some(MessageId::new()),
                blob_id: Some(BlobId::of(format!("message {seq}").as_bytes())),
                actor: None,
            }
            .seal(seq, prev);
            prev = entry.hash;
            entry
        })
        .collect()
}

fn check(entries: &[LedgerEntry]) -> ChainReport {
    let mut check = ChainCheck::new();
    for entry in entries {
        check.push(entry);
    }
    check.finish()
}

#[test]
fn an_untouched_chain_has_no_problems() {
    let entries = chain(5);
    let report = check(&entries);
    assert_eq!(report.entries, 5);
    assert_eq!(report.head, Some((5, entries[4].hash)));
    assert!(report.problems.is_empty());
    assert_eq!(check(&[]).head, None);
}

#[test]
fn every_field_is_sealed() {
    let entry = chain(1).remove(0);
    let changes: [fn(&mut LedgerEntry); 8] = [
        |e| e.seq += 1,
        |e| e.at = Timestamp::from_millis(e.at.as_millis() + 1),
        |e| e.event = LedgerEvent::Deleted,
        |e| e.archive_id = ArchiveId::new(),
        |e| e.message_id = None,
        |e| e.blob_id = Some(BlobId::of(b"other bytes")),
        |e| e.actor = Some(UserId::new()),
        |e| e.prev = LedgerHash::from([1; 32]),
    ];
    for change in changes {
        let mut changed = entry.clone();
        change(&mut changed);
        assert_ne!(changed.expected_hash(), entry.hash);
    }
}

#[test]
fn names_changed_missing_and_reordered_entries() {
    let entries = chain(5);

    let mut changed = entries.clone();
    changed[2].blob_id = Some(BlobId::of(b"swapped"));
    assert_eq!(check(&changed).problems, [ChainProblem::Altered { seq: 3 }]);

    let mut resealed = entries.clone();
    resealed[2].blob_id = Some(BlobId::of(b"swapped"));
    resealed[2].hash = resealed[2].expected_hash();
    assert_eq!(
        check(&resealed).problems,
        [ChainProblem::BrokenLink { seq: 4 }]
    );

    let mut missing = entries.clone();
    missing.remove(1);
    assert_eq!(
        check(&missing).problems,
        [
            ChainProblem::Gap {
                expected: 2,
                found: 3
            },
            ChainProblem::BrokenLink { seq: 3 }
        ]
    );
}

#[test]
fn checks_a_written_down_head() {
    let entries = chain(4);
    let mut good = ChainCheck::new().with_anchor(3, entries[2].hash);
    let mut wrong = ChainCheck::new().with_anchor(3, entries[1].hash);
    let mut later = ChainCheck::new().with_anchor(9, entries[1].hash);
    for entry in &entries {
        good.push(entry);
        wrong.push(entry);
        later.push(entry);
    }
    assert!(good.finish().problems.is_empty());
    assert_eq!(
        wrong.finish().problems,
        [ChainProblem::AnchorMismatch { seq: 3 }]
    );
    assert_eq!(
        later.finish().problems,
        [ChainProblem::AnchorMissing { seq: 9 }]
    );
}

#[test]
fn keeps_a_bounded_list_of_problems() {
    let mut entries = chain(150);
    for entry in &mut entries {
        entry.at = Timestamp::from_millis(0);
    }
    let report = check(&entries);
    assert_eq!(report.problems.len(), MAX_PROBLEMS);
    assert_eq!(report.problem_count, 150);
}

#[test]
fn hashes_read_back_as_written() {
    let hash = chain(1)[0].hash;
    assert_eq!(hash.to_string().len(), 64);
    assert_eq!(hash.to_string().parse::<LedgerHash>().unwrap(), hash);
    assert_eq!(
        hash.to_string()
            .to_uppercase()
            .parse::<LedgerHash>()
            .unwrap(),
        hash
    );
    for bad in ["", "zz", &"0".repeat(63), &"g".repeat(64)] {
        assert!(bad.parse::<LedgerHash>().is_err(), "{bad}");
    }
}
