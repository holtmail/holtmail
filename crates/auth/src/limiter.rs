use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_KEYS: usize = 100_000;

pub(crate) struct Limiter {
    max_failures: u32,
    window: Duration,
    failures: Mutex<HashMap<(IpAddr, String), VecDeque<Instant>>>,
}

impl Limiter {
    pub(crate) fn new(max_failures: u32, window: Duration) -> Self {
        Self {
            max_failures,
            window,
            failures: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(IpAddr, String), VecDeque<Instant>>> {
        self.failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `Some(wait)` when the key used up its failures within the window.
    pub(crate) fn blocked(&self, ip: IpAddr, email: &str, now: Instant) -> Option<Duration> {
        let mut map = self.lock();
        let key = (ip, email.to_owned());
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

    pub(crate) fn fail(&self, ip: IpAddr, email: &str, now: Instant) {
        let mut map = self.lock();
        if map.len() >= MAX_KEYS {
            let window = self.window;
            map.retain(|_, times| {
                times
                    .back()
                    .is_some_and(|t| now.duration_since(*t) < window)
            });
        }
        let times = map.entry((ip, email.to_owned())).or_default();
        times.push_back(now);
        while times.len() > self.max_failures as usize {
            times.pop_front();
        }
    }

    pub(crate) fn clear(&self, ip: IpAddr, email: &str) {
        self.lock().remove(&(ip, email.to_owned()));
    }

    pub(crate) fn prune(&self, now: Instant) {
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
}
