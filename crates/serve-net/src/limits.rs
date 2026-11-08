use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use holtmail_core::{UserId, address_block};

/// How many connections may be open at once. A limit of zero refuses every connection it
/// applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// All connections.
    pub total: usize,
    /// Signed-in connections of one user.
    pub per_user: usize,
    /// Connections not signed in, per client address. IPv6 clients count per `/64`.
    pub per_address: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            total: 200,
            per_user: 20,
            per_address: 10,
        }
    }
}

/// Which limit refused a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LimitExceeded {
    /// [`Limits::total`].
    #[error("too many connections")]
    Total,
    /// [`Limits::per_user`].
    #[error("too many connections for this user")]
    PerUser,
    /// [`Limits::per_address`].
    #[error("too many connections from this address")]
    PerAddress,
}

#[derive(Debug, Default)]
struct Counts {
    total: usize,
    addresses: HashMap<IpAddr, usize>,
    users: HashMap<UserId, usize>,
}

#[derive(Debug)]
struct Shared {
    limits: Limits,
    counts: Mutex<Counts>,
}

impl Shared {
    fn counts(&self) -> MutexGuard<'_, Counts> {
        self.counts.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The connection counts of one protocol. Listeners that share a limiter (say 993 and 143)
/// share its limits; every protocol has its own. Cloning shares the counts.
#[derive(Debug, Clone)]
pub struct ConnectionLimiter(Arc<Shared>);

/// A connection's place in the limits, for a front end that hands the connection's bytes to
/// another layer and still has to record who signed in. The place is freed when the
/// [`Connection`](crate::Connection) and every clone of this are gone.
#[derive(Clone)]
pub struct LimitPlace(pub(crate) Arc<Mutex<ConnectionSlot>>);

impl LimitPlace {
    /// [`Connection::authenticated`](crate::Connection::authenticated), from outside the
    /// connection.
    pub fn authenticated(&self, user: &UserId) -> Result<(), LimitExceeded> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .authenticate(user)
    }
}

impl std::fmt::Debug for LimitPlace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LimitPlace(..)")
    }
}

impl ConnectionLimiter {
    /// A limiter with no connections open.
    pub fn new(limits: Limits) -> Self {
        Self(Arc::new(Shared {
            limits,
            counts: Mutex::default(),
        }))
    }

    /// How many connections are open right now.
    pub fn open(&self) -> usize {
        self.0.counts().total
    }

    pub(crate) fn acquire(&self) -> Result<ConnectionSlot, LimitExceeded> {
        let mut counts = self.0.counts();
        if counts.total >= self.0.limits.total {
            return Err(LimitExceeded::Total);
        }
        counts.total += 1;
        Ok(ConnectionSlot {
            shared: self.0.clone(),
            held: Held::Counted,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Held {
    Counted,
    Address(IpAddr),
    User(UserId),
}

#[derive(Debug)]
pub(crate) struct ConnectionSlot {
    shared: Arc<Shared>,
    held: Held,
}

impl ConnectionSlot {
    pub(crate) fn bind_address(&mut self, ip: IpAddr) -> Result<(), LimitExceeded> {
        let key = address_block(ip);
        let mut counts = self.shared.counts();
        let count = counts.addresses.entry(key).or_default();
        if *count >= self.shared.limits.per_address {
            if *count == 0 {
                counts.addresses.remove(&key);
            }
            return Err(LimitExceeded::PerAddress);
        }
        *count += 1;
        drop(counts);
        self.held = Held::Address(key);
        Ok(())
    }

    pub(crate) fn authenticate(&mut self, user: &UserId) -> Result<(), LimitExceeded> {
        if self.held == Held::User(*user) {
            return Ok(());
        }
        let mut counts = self.shared.counts();
        let count = counts.users.entry(*user).or_default();
        if *count >= self.shared.limits.per_user {
            if *count == 0 {
                counts.users.remove(user);
            }
            return Err(LimitExceeded::PerUser);
        }
        *count += 1;
        release(&mut counts, &self.held);
        drop(counts);
        self.held = Held::User(*user);
        Ok(())
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        let mut counts = self.shared.counts();
        counts.total -= 1;
        release(&mut counts, &self.held);
    }
}

fn release(counts: &mut Counts, held: &Held) {
    match held {
        Held::Counted => {}
        Held::Address(key) => decrement(&mut counts.addresses, *key),
        Held::User(user) => decrement(&mut counts.users, *user),
    }
}

fn decrement<K: Eq + Hash>(map: &mut HashMap<K, usize>, key: K) {
    if let Entry::Occupied(mut entry) = map.entry(key) {
        *entry.get_mut() -= 1;
        if *entry.get() == 0 {
            entry.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(total: usize, per_user: usize, per_address: usize) -> ConnectionLimiter {
        ConnectionLimiter::new(Limits {
            total,
            per_user,
            per_address,
        })
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn total_is_counted_and_released() {
        let limiter = limiter(2, 9, 9);
        let a = limiter.acquire().unwrap();
        let _b = limiter.acquire().unwrap();
        assert_eq!(limiter.acquire().unwrap_err(), LimitExceeded::Total);
        assert_eq!(limiter.open(), 2);
        drop(a);
        assert_eq!(limiter.open(), 1);
        assert!(limiter.acquire().is_ok());
    }

    #[test]
    fn an_address_is_limited_alone() {
        let limiter = limiter(9, 9, 1);
        let mut a = limiter.acquire().unwrap();
        a.bind_address(ip("203.0.113.1")).unwrap();
        let mut b = limiter.acquire().unwrap();
        assert_eq!(
            b.bind_address(ip("203.0.113.1")),
            Err(LimitExceeded::PerAddress)
        );
        b.bind_address(ip("203.0.113.2")).unwrap();
        drop(a);
        let mut c = limiter.acquire().unwrap();
        c.bind_address(ip("203.0.113.1")).unwrap();
    }

    #[test]
    fn ipv6_counts_per_64_and_mapped_counts_as_ipv4() {
        let limiter = limiter(9, 9, 1);
        let mut a = limiter.acquire().unwrap();
        a.bind_address(ip("2001:db8:1:1::1")).unwrap();
        let mut b = limiter.acquire().unwrap();
        assert!(b.bind_address(ip("2001:db8:1:1::2")).is_err());
        b.bind_address(ip("2001:db8:1:2::1")).unwrap();
        let mut c = limiter.acquire().unwrap();
        c.bind_address(ip("203.0.113.1")).unwrap();
        let mut d = limiter.acquire().unwrap();
        assert!(d.bind_address(ip("::ffff:203.0.113.1")).is_err());
    }

    #[test]
    fn signing_in_frees_the_address_place() {
        let limiter = limiter(9, 9, 1);
        let mut a = limiter.acquire().unwrap();
        a.bind_address(ip("203.0.113.1")).unwrap();
        a.authenticate(&UserId::new()).unwrap();
        let mut b = limiter.acquire().unwrap();
        b.bind_address(ip("203.0.113.1")).unwrap();
    }

    #[test]
    fn a_user_is_limited_and_a_refusal_changes_nothing() {
        let (first, second) = (UserId::new(), UserId::new());
        let limiter = limiter(9, 1, 2);
        let mut a = limiter.acquire().unwrap();
        a.bind_address(ip("203.0.113.1")).unwrap();
        a.authenticate(&first).unwrap();
        let mut b = limiter.acquire().unwrap();
        b.bind_address(ip("203.0.113.1")).unwrap();
        assert_eq!(b.authenticate(&first), Err(LimitExceeded::PerUser));
        let mut c = limiter.acquire().unwrap();
        c.bind_address(ip("203.0.113.1")).unwrap();
        let mut d = limiter.acquire().unwrap();
        assert_eq!(
            d.bind_address(ip("203.0.113.1")),
            Err(LimitExceeded::PerAddress),
            "the refused connection still holds its address place"
        );
        b.authenticate(&second).unwrap();
        d.bind_address(ip("203.0.113.1")).unwrap();
    }

    #[test]
    fn signing_in_again_counts_once() {
        let (first, second) = (UserId::new(), UserId::new());
        let limiter = limiter(9, 1, 9);
        let mut a = limiter.acquire().unwrap();
        a.authenticate(&first).unwrap();
        a.authenticate(&first).unwrap();
        a.authenticate(&second).unwrap();
        let mut b = limiter.acquire().unwrap();
        b.authenticate(&first).unwrap();
        drop(a);
        let mut c = limiter.acquire().unwrap();
        c.authenticate(&second).unwrap();
    }

    #[test]
    fn no_entry_is_left_behind() {
        let limiter = limiter(9, 0, 0);
        let mut a = limiter.acquire().unwrap();
        assert!(a.bind_address(ip("203.0.113.1")).is_err());
        assert!(a.authenticate(&UserId::new()).is_err());
        let counts = limiter.0.counts();
        assert!(counts.addresses.is_empty() && counts.users.is_empty());
    }
}
