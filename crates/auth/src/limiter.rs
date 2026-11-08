use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use holtmail_core::address_block;

const MAX_KEYS: usize = 100_000;

type Key = (IpAddr, String);

/// Counts failed sign-ins per client and login name, in memory.
///
/// A client is its [`address_block`]: an IPv4 address, or an IPv6 /64, however it is written.
/// Failures older than the window do not count. Safe to share between tasks.
///
/// Memory stays bounded at 100 000 keys. A new key arriving at that size first drops the keys
/// with no failure left in the window, then, if that frees less than a tenth, the keys whose last
/// failure is oldest, down to nine tenths. A client can therefore lose its count only after
/// thousands of newer keys have failed.
pub struct Limiter {
    max_failures: u32,
    window: Duration,
    capacity: usize,
    failures: Mutex<HashMap<Key, VecDeque<Instant>>>,
}

fn key(ip: IpAddr, email: &str) -> Key {
    (address_block(ip), email.to_owned())
}

impl Limiter {
    /// A limiter that blocks a key after `max_failures` failures within `window`.
    pub fn new(max_failures: u32, window: Duration) -> Self {
        Self::with_capacity(max_failures, window, MAX_KEYS)
    }

    fn with_capacity(max_failures: u32, window: Duration, capacity: usize) -> Self {
        Self {
            max_failures,
            window,
            capacity,
            failures: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, VecDeque<Instant>>> {
        self.failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `Some(wait)` when the key used up its failures within the window, `wait` being at least
    /// one second; `None` when it may try.
    pub fn blocked(&self, ip: IpAddr, email: &str, now: Instant) -> Option<Duration> {
        let mut map = self.lock();
        let key = key(ip, email);
        let times = map.get_mut(&key)?;
        while times
            .front()
            .is_some_and(|t| now.duration_since(*t) >= self.window)
        {
            times.pop_front();
        }
        if times.is_empty() {
            map.remove(&key);
            return None;
        }
        if times.len() < self.max_failures as usize {
            return None;
        }
        let oldest = *times.front()?;
        Some(
            self.window
                .saturating_sub(now.duration_since(oldest))
                .max(Duration::from_secs(1)),
        )
    }

    /// Frees at least a tenth of the capacity, so the scan is paid once per that many new keys.
    fn make_room(&self, map: &mut HashMap<Key, VecDeque<Instant>>, now: Instant) {
        let window = self.window;
        map.retain(|_, times| {
            times
                .back()
                .is_some_and(|t| now.duration_since(*t) < window)
        });
        let keep = self.capacity - self.capacity.div_ceil(10);
        if map.len() <= keep {
            return;
        }
        let mut latest: Vec<Instant> = map.values().filter_map(|t| t.back().copied()).collect();
        let (_, cutoff, _) = latest.select_nth_unstable(map.len() - keep - 1);
        let cutoff = *cutoff;
        map.retain(|_, times| times.back().is_some_and(|t| *t > cutoff));
    }

    /// Counts a failure of the key at `now`.
    pub fn fail(&self, ip: IpAddr, email: &str, now: Instant) {
        let mut map = self.lock();
        let key = key(ip, email);
        if map.len() >= self.capacity && !map.contains_key(&key) {
            self.make_room(&mut map, now);
        }
        let times = map.entry(key).or_default();
        times.push_back(now);
        while times.len() > self.max_failures as usize {
            times.pop_front();
        }
    }

    /// Forgets the key's failures.
    pub fn clear(&self, ip: IpAddr, email: &str) {
        self.lock().remove(&key(ip, email));
    }

    /// Drops every key with no failure left in the window at `now`.
    pub fn prune(&self, now: Instant) {
        let window = self.window;
        self.lock().retain(|_, times| {
            times
                .back()
                .is_some_and(|t| now.duration_since(*t) < window)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_too_many_failures_until_the_window_passes() {
        let limiter = Limiter::new(3, Duration::from_secs(60));
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let start = Instant::now();
        for n in 0..3 {
            assert_eq!(limiter.blocked(ip, "ada@example.com", start), None, "{n}");
            limiter.fail(ip, "ada@example.com", start + Duration::from_secs(n));
        }
        let wait = limiter
            .blocked(ip, "ada@example.com", start + Duration::from_secs(10))
            .unwrap();
        assert_eq!(wait, Duration::from_secs(50));
        assert_eq!(limiter.blocked(ip, "bob@example.com", start), None);
        assert_eq!(
            limiter.blocked("192.0.2.2".parse().unwrap(), "ada@example.com", start),
            None
        );
        assert_eq!(
            limiter.blocked(ip, "ada@example.com", start + Duration::from_secs(60)),
            None,
            "the oldest failure left the window"
        );
        limiter.clear(ip, "ada@example.com");
        limiter.prune(start);
        assert!(limiter.lock().is_empty());
    }

    #[test]
    fn one_ipv6_block_is_one_client() {
        let limiter = Limiter::new(2, Duration::from_secs(60));
        let start = Instant::now();
        limiter.fail("2001:db8:1:2::a".parse().unwrap(), "ada@example.com", start);
        limiter.fail("2001:db8:1:2::b".parse().unwrap(), "ada@example.com", start);
        assert!(
            limiter
                .blocked(
                    "2001:db8:1:2:ffff::c".parse().unwrap(),
                    "ada@example.com",
                    start
                )
                .is_some(),
            "another address of the /64"
        );
        assert_eq!(
            limiter.blocked("2001:db8:1:3::a".parse().unwrap(), "ada@example.com", start),
            None,
            "another /64"
        );
    }

    #[test]
    fn a_mapped_ipv4_address_is_that_ipv4_address() {
        let limiter = Limiter::new(1, Duration::from_secs(60));
        let start = Instant::now();
        limiter.fail("192.0.2.9".parse().unwrap(), "ada@example.com", start);
        assert!(
            limiter
                .blocked(
                    "::ffff:192.0.2.9".parse().unwrap(),
                    "ada@example.com",
                    start
                )
                .is_some()
        );
    }

    #[test]
    fn stays_under_capacity_with_every_key_in_the_window() {
        let limiter = Limiter::with_capacity(3, Duration::from_secs(3600), 100);
        let start = Instant::now();
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let mut largest = 0;
        for n in 0..1000u64 {
            limiter.fail(
                ip,
                &format!("u{n}@example.com"),
                start + Duration::from_secs(n),
            );
            largest = largest.max(limiter.lock().len());
        }
        assert!(largest <= 100, "{largest}");
        let now = start + Duration::from_secs(1000);
        limiter.fail(ip, "u999@example.com", now);
        limiter.fail(ip, "u999@example.com", now);
        assert!(
            limiter.blocked(ip, "u999@example.com", now).is_some(),
            "the newest keys stay"
        );
        assert!(!limiter.lock().contains_key(&key(ip, "u0@example.com")));
    }

    #[test]
    fn failing_again_on_a_known_key_at_capacity_evicts_nothing() {
        let limiter = Limiter::with_capacity(3, Duration::from_secs(3600), 10);
        let start = Instant::now();
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        for n in 0..10u64 {
            limiter.fail(
                ip,
                &format!("u{n}@example.com"),
                start + Duration::from_secs(n),
            );
        }
        limiter.fail(ip, "u0@example.com", start + Duration::from_secs(20));
        assert_eq!(limiter.lock().len(), 10);
    }
}
