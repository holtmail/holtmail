use holtmail_insight::Reservation;

/// The kind of a reservation item. Not persisted: stage B rebuilds it from the stored items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReservationKind {
    /// [`Reservation::Flight`].
    Flight = 0,
    /// [`Reservation::Lodging`].
    Lodging = 1,
    /// [`Reservation::Train`].
    Train = 2,
    /// [`Reservation::Bus`].
    Bus = 3,
    /// [`Reservation::Event`].
    Event = 4,
    /// [`Reservation::Food`].
    Food = 5,
    /// [`Reservation::RentalCar`].
    RentalCar = 6,
}

impl ReservationKind {
    /// Every kind, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::Flight,
        Self::Lodging,
        Self::Train,
        Self::Bus,
        Self::Event,
        Self::Food,
        Self::RentalCar,
    ];

    /// The snake_case name, as the items JSON names the reservation type.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Flight => "flight",
            Self::Lodging => "lodging",
            Self::Train => "train",
            Self::Bus => "bus",
            Self::Event => "event",
            Self::Food => "food",
            Self::RentalCar => "rental_car",
        }
    }

    const fn mask(self) -> u8 {
        1 << self as u8
    }
}

impl From<&Reservation> for ReservationKind {
    fn from(reservation: &Reservation) -> Self {
        match reservation {
            Reservation::Flight(_) => Self::Flight,
            Reservation::Lodging(_) => Self::Lodging,
            Reservation::Train(_) => Self::Train,
            Reservation::Bus(_) => Self::Bus,
            Reservation::Event(_) => Self::Event,
            Reservation::Food(_) => Self::Food,
            Reservation::RentalCar(_) => Self::RentalCar,
        }
    }
}

/// The reservation kinds of one message: the same items as `kinds` (at `Likely` or higher and
/// not forwarded), split by reservation type. Not persisted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ReservationKinds(u8);

impl ReservationKinds {
    /// The empty set.
    pub const EMPTY: Self = Self(0);

    /// Whether the set is empty.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `kind` is in the set.
    pub const fn contains(self, kind: ReservationKind) -> bool {
        self.0 & kind.mask() != 0
    }

    /// Adds `kind`.
    pub fn insert(&mut self, kind: ReservationKind) {
        self.0 |= kind.mask();
    }

    /// Removes `kind`.
    pub fn remove(&mut self, kind: ReservationKind) {
        self.0 &= !kind.mask();
    }

    /// The set with `kind` added.
    #[must_use]
    pub const fn with(self, kind: ReservationKind) -> Self {
        Self(self.0 | kind.mask())
    }

    /// The kinds in the set, in declaration order.
    pub fn iter(self) -> impl Iterator<Item = ReservationKind> {
        ReservationKind::ALL
            .iter()
            .copied()
            .filter(move |kind| self.contains(*kind))
    }
}

impl FromIterator<ReservationKind> for ReservationKinds {
    fn from_iter<I: IntoIterator<Item = ReservationKind>>(iter: I) -> Self {
        iter.into_iter().fold(Self::EMPTY, Self::with)
    }
}

impl From<ReservationKind> for ReservationKinds {
    fn from(kind: ReservationKind) -> Self {
        Self::EMPTY.with(kind)
    }
}
