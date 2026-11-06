use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! bit_set {
    (
        $(#[$tag_meta:meta])* $tag:ident;
        $(#[$set_meta:meta])* $set:ident;
        $($(#[$variant_meta:meta])* $variant:ident = $bit:literal, $name:literal;)+
    ) => {
        $(#[$tag_meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $tag {
            $($(#[$variant_meta])* $variant = $bit,)+
        }

        impl $tag {
            /// Every member, in bit order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            /// The persisted bit position. Never changes and is never reused.
            pub const fn bit(self) -> u32 {
                self as u32
            }

            /// The snake_case name.
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)+
                }
            }

            /// The member with this snake_case name.
            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$variant),)+
                    _ => None,
                }
            }

            /// The member at this bit position.
            pub fn from_bit(bit: u32) -> Option<Self> {
                match bit {
                    $($bit => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }

        impl fmt::Display for $tag {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.name())
            }
        }

        $(#[$set_meta])*
        ///
        /// Stored as an integer of its bits. Bits this build does not know are kept as they are, so
        /// a value written by a newer build survives a round trip.
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $set(u64);

        impl $set {
            /// The empty set.
            pub const EMPTY: Self = Self(0);

            /// The set with exactly these bits, unknown ones included.
            pub const fn from_bits(bits: u64) -> Self {
                Self(bits)
            }

            /// The stored bits.
            pub const fn bits(self) -> u64 {
                self.0
            }

            /// Whether the set holds no bit at all.
            pub const fn is_empty(self) -> bool {
                self.0 == 0
            }

            /// Whether `member` is in the set.
            pub const fn contains(self, member: $tag) -> bool {
                self.0 & (1 << member.bit()) != 0
            }

            /// Adds `member`.
            pub fn insert(&mut self, member: $tag) {
                self.0 |= 1 << member.bit();
            }

            /// Removes `member`.
            pub fn remove(&mut self, member: $tag) {
                self.0 &= !(1 << member.bit());
            }

            /// The set with `member` added.
            #[must_use]
            pub const fn with(self, member: $tag) -> Self {
                Self(self.0 | (1 << member.bit()))
            }

            /// The known members in the set, in bit order.
            pub fn iter(self) -> impl Iterator<Item = $tag> {
                $tag::ALL.iter().copied().filter(move |member| self.contains(*member))
            }
        }

        impl FromIterator<$tag> for $set {
            fn from_iter<I: IntoIterator<Item = $tag>>(iter: I) -> Self {
                iter.into_iter().fold(Self::EMPTY, Self::with)
            }
        }

        impl From<$tag> for $set {
            fn from(member: $tag) -> Self {
                Self::EMPTY.with(member)
            }
        }

        impl std::ops::BitOr for $set {
            type Output = Self;

            fn bitor(self, other: Self) -> Self {
                Self(self.0 | other.0)
            }
        }

        impl std::ops::BitOrAssign for $set {
            fn bitor_assign(&mut self, other: Self) {
                self.0 |= other.0;
            }
        }
    };
}

bit_set! {
    /// One header signal computed in stage A. Bit positions are append-only.
    Signal;
    /// The header signals of one message (the `signals` column).
    Signals;
    /// A `List-Id` field is present.
    ListId = 0, "list_id";
    /// A `List-Post` field other than `NO` is present.
    ListPost = 1, "list_post";
    /// `List-Post: NO`, comments ignored.
    ListPostNo = 2, "list_post_no";
    /// A `List-Unsubscribe` field is present.
    Unsubscribe = 3, "unsubscribe";
    /// A `List-Unsubscribe-Post` field is present.
    UnsubscribePost = 4, "unsubscribe_post";
    /// `Precedence: bulk`, `list` or `junk`.
    PrecedenceBulk = 5, "precedence_bulk";
    /// `Auto-Submitted` other than `no` and `auto-replied`.
    AutoSubmitted = 6, "auto_submitted";
    /// `Auto-Submitted: auto-replied`.
    AutoReplied = 7, "auto_replied";
    /// A `Feedback-ID` field is present.
    FeedbackId = 8, "feedback_id";
    /// An email service provider's fingerprint field is present.
    Esp = 9, "esp";
    /// The sender's local part is an automated one (`noreply` and the like).
    Noreply = 10, "noreply";
    /// The body has an inline-forward marker.
    ForwardMarker = 11, "forward_marker";
    /// Structured data (JSON-LD, microdata or a pass) was found.
    Structured = 12, "structured";
}

bit_set! {
    /// The kind of an insight item, as stored. Bit positions are append-only.
    KindTag;
    /// The item kinds of one message (the `kinds` column): kinds present at `Likely` or higher
    /// and not forwarded.
    KindTags;
    /// [`InsightKind::Code`](crate::InsightKind::Code).
    Code = 0, "code";
    /// [`InsightKind::Action`](crate::InsightKind::Action).
    Link = 1, "link";
    /// [`InsightKind::Unsubscribe`](crate::InsightKind::Unsubscribe).
    Unsubscribe = 2, "unsubscribe";
    /// [`InsightKind::Reservation`](crate::InsightKind::Reservation).
    Reservation = 3, "reservation";
    /// [`InsightKind::Order`](crate::InsightKind::Order).
    Order = 4, "order";
    /// [`InsightKind::Invoice`](crate::InsightKind::Invoice).
    Invoice = 5, "invoice";
    /// [`InsightKind::Parcel`](crate::InsightKind::Parcel).
    Parcel = 6, "parcel";
    /// [`InsightKind::Pass`](crate::InsightKind::Pass).
    Pass = 7, "pass";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_bits_never_move() {
        let signals: Vec<_> = Signal::ALL.iter().map(|s| (s.bit(), s.name())).collect();
        assert_eq!(
            signals,
            [
                (0, "list_id"),
                (1, "list_post"),
                (2, "list_post_no"),
                (3, "unsubscribe"),
                (4, "unsubscribe_post"),
                (5, "precedence_bulk"),
                (6, "auto_submitted"),
                (7, "auto_replied"),
                (8, "feedback_id"),
                (9, "esp"),
                (10, "noreply"),
                (11, "forward_marker"),
                (12, "structured"),
            ]
        );
        let kinds: Vec<_> = KindTag::ALL.iter().map(|k| (k.bit(), k.name())).collect();
        assert_eq!(
            kinds,
            [
                (0, "code"),
                (1, "link"),
                (2, "unsubscribe"),
                (3, "reservation"),
                (4, "order"),
                (5, "invoice"),
                (6, "parcel"),
                (7, "pass"),
            ]
        );
    }

    #[test]
    fn sets_keep_unknown_bits() {
        let stored = Signals::from_bits((1 << 40) | 1);
        assert!(stored.contains(Signal::ListId));
        assert_eq!(stored.iter().collect::<Vec<_>>(), [Signal::ListId]);
        let json = serde_json::to_string(&stored).unwrap();
        assert_eq!(serde_json::from_str::<Signals>(&json).unwrap(), stored);
    }

    #[test]
    fn sets_insert_and_remove() {
        let mut kinds: KindTags = [KindTag::Code, KindTag::Pass].into_iter().collect();
        assert_eq!(kinds.bits(), 0b1000_0001);
        kinds.remove(KindTag::Code);
        kinds.insert(KindTag::Order);
        assert_eq!(
            kinds.iter().collect::<Vec<_>>(),
            [KindTag::Order, KindTag::Pass]
        );
        assert_eq!(KindTag::from_name("parcel"), Some(KindTag::Parcel));
        assert_eq!(KindTag::from_bit(6), Some(KindTag::Parcel));
        assert_eq!(Signal::from_bit(13), None);
    }
}
