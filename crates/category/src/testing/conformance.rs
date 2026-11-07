//! The suite every [`Categorizer`] runs in its own tests.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{
    Categorizer, CategoryInput, CategorySnapshot, CategoryVote, KindTag, KindTags, ReservationKind,
    ReservationKinds, Signal, Signals, TrustState,
};

const KNOWN_SIGNAL_BITS: u32 = 13;
const LONG_SENDER_BYTES: usize = 10 * 1024;

/// Checks that `categorizer` keeps the [`Categorizer`] contract. Panics on the first violation,
/// naming the case and the rule.
///
/// `cases` are the implementation's own inputs with the vote each must give. The suite checks:
///
/// - `id()` is a non-empty lowercase name and stays equal;
/// - two snapshots taken with no reload between them have equal, non-empty versions, and each
///   snapshot's version stays equal;
/// - every case gives its expected vote, equal on two runs and on both snapshots;
/// - a hostile set of inputs never panics, gives equal votes on two runs, and never gives
///   unknown topic bits: every combination of the 13 known [`Signal`] bits (with the other fields
///   varied deterministically), unknown high bits in `signals` and `kinds`, 10 KiB, non-ASCII and
///   empty senders, no sender, and every [`TrustState`].
pub fn categorizer(categorizer: &dyn Categorizer, cases: &[(CategoryInput, Option<CategoryVote>)]) {
    let id = categorizer.id().to_owned();
    assert!(
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'),
        "id {id:?} is not a lowercase name"
    );
    let first = categorizer.snapshot();
    let second = categorizer.snapshot();
    let version = first.version();
    assert!(!version.is_empty(), "{id}: empty version");
    assert_eq!(first.version(), version, "{id}: snapshot version changed");
    assert_eq!(
        second.version(),
        version,
        "{id}: two snapshots differ in version"
    );

    for (index, (input, expected)) in cases.iter().enumerate() {
        let name = format!("{id}/case {index}");
        let vote = run(first.as_ref(), input, &name);
        assert_eq!(&vote, expected, "{name}: unexpected vote");
        assert_eq!(
            run(second.as_ref(), input, &name),
            vote,
            "{name}: snapshots differ"
        );
    }
    for (index, input) in hostile_inputs().iter().enumerate() {
        run(first.as_ref(), input, &format!("{id}/hostile {index}"));
    }

    assert_eq!(first.version(), version, "{id}: snapshot version changed");
    assert_eq!(categorizer.id(), id, "id changed");
}

fn run(snapshot: &dyn CategorySnapshot, input: &CategoryInput, name: &str) -> Option<CategoryVote> {
    let call = || {
        catch_unwind(AssertUnwindSafe(|| snapshot.categorize(input)))
            .unwrap_or_else(|_| panic!("{name}: categorize panicked"))
    };
    let vote = call();
    assert_eq!(call(), vote, "{name}: two runs differ");
    if let Some(vote) = &vote {
        assert_eq!(
            vote.topics,
            vote.topics.known(),
            "{name}: unknown topic bits in the vote"
        );
    }
    vote
}

fn trust_states() -> Vec<TrustState> {
    vec![
        TrustState::Aligned {
            domain: "example.com".into(),
        },
        TrustState::Relayed {
            provider: "relay.example.org".into(),
        },
        TrustState::Unverified,
        TrustState::Unknown,
    ]
}

fn senders() -> Vec<(Option<String>, Option<String>)> {
    let long = format!(
        "{}@{}",
        "a".repeat(LONG_SENDER_BYTES),
        "b".repeat(LONG_SENDER_BYTES)
    );
    vec![
        (None, None),
        (Some(String::new()), Some(String::new())),
        (Some("ada@example.com".into()), Some("example.com".into())),
        (Some(long.clone()), Some(long)),
        (
            Some("ádá@пример.example.org".into()),
            Some("пример.example.org".into()),
        ),
        (Some("@".into()), Some(".".into())),
        (
            Some("\u{0}\u{feff}".into()),
            Some("\u{202e}moc.elpmaxe".into()),
        ),
    ]
}

/// The hostile inputs, built deterministically.
fn hostile_inputs() -> Vec<CategoryInput> {
    let trusts = trust_states();
    let senders = senders();
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut out = Vec::new();
    for bits in 0..(1u64 << KNOWN_SIGNAL_BITS) {
        let r = next();
        let (sender_address, sender_domain) = senders[(r % senders.len() as u64) as usize].clone();
        out.push(CategoryInput {
            signals: Signals::from_bits(bits),
            kinds: KindTags::from_bits((r >> 8) & 0xff),
            reservations: ReservationKind::ALL
                .iter()
                .copied()
                .filter(|kind| (r >> (16 + *kind as u32)) & 1 == 1)
                .collect(),
            structured: (r >> 24) & 1 == 1,
            forwarded: (r >> 25) & 1 == 1,
            own: (r >> 26) & 1 == 1,
            trust: trusts[((r >> 27) % trusts.len() as u64) as usize].clone(),
            sender_address,
            sender_domain,
        });
    }
    let all_reservations: ReservationKinds = ReservationKind::ALL.iter().copied().collect();
    let all_kinds: KindTags = KindTag::ALL.iter().copied().collect();
    let all_signals: Signals = Signal::ALL.iter().copied().collect();
    for trust in &trusts {
        for (sender_address, sender_domain) in &senders {
            for (signals, kinds) in [
                (Signals::from_bits(u64::MAX), KindTags::from_bits(u64::MAX)),
                (
                    Signals::from_bits(all_signals.bits() | (1 << 63)),
                    KindTags::from_bits(all_kinds.bits() | (1 << 40)),
                ),
                (Signals::from_bits(1 << 50), KindTags::from_bits(1 << 50)),
                (Signals::EMPTY, KindTags::EMPTY),
            ] {
                for flags in 0..8u8 {
                    out.push(CategoryInput {
                        signals,
                        kinds,
                        reservations: if flags & 1 == 1 {
                            all_reservations
                        } else {
                            ReservationKinds::EMPTY
                        },
                        structured: flags & 1 == 1,
                        forwarded: flags & 2 == 2,
                        own: flags & 4 == 4,
                        trust: trust.clone(),
                        sender_address: sender_address.clone(),
                        sender_domain: sender_domain.clone(),
                    });
                }
            }
        }
    }
    out
}
