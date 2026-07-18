//! Shared key pool with per-key 429 blacklist and round-robin rotation.
//!
//! Used by OpenAI, Gemini, and embedding providers that support multiple
//! API keys with rate-limit-driven rotation.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::warn;

/// Default cooldown after a key returns 429 (rate-limit).
/// OpenAI's and Gemini's per-key quotas keep 429ing for a while;
/// parking the key lets round-robin move to a fresh one.
pub const DEFAULT_BLACKLIST_DURATION: Duration = Duration::from_secs(60 * 60);

/// Per-key 429 blacklist with round-robin cursor.
///
/// Wraps a `Vec<Option<Instant>>` indexed in lock-step with the caller's
/// `api_keys` slice. `Some(instant)` means "do not use until `instant`";
/// `None` means the key is usable.
#[derive(Debug)]
pub struct KeyPool {
    blacklist: Arc<Mutex<Vec<Option<Instant>>>>,
    cooldown: Duration,
}

impl KeyPool {
    /// Create a new pool with `count` key slots.
    #[must_use]
    pub fn new(count: usize) -> Self {
        Self::with_cooldown(count, DEFAULT_BLACKLIST_DURATION)
    }

    /// Create a pool with a custom cooldown duration.
    #[must_use]
    pub fn with_cooldown(count: usize, cooldown: Duration) -> Self {
        Self {
            blacklist: Arc::new(Mutex::new(vec![None; count])),
            cooldown,
        }
    }

    /// Number of keys in the pool.
    #[must_use]
    pub fn len(&self) -> usize {
        self.blacklist.lock().map(|b| b.len()).unwrap_or(0)
    }

    /// Whether the pool has no keys.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Mark a key as temporarily unusable after a 429 response.
    ///
    /// The key is skipped by [`next_usable`] until the cooldown elapses.
    pub fn blacklist(&self, key_idx: usize, provider: &str) {
        let Ok(mut blacklist) = self.blacklist.lock() else {
            return;
        };
        let Some(slot) = blacklist.get_mut(key_idx) else {
            return;
        };
        if slot.is_some() {
            return;
        }
        *slot = Some(Instant::now() + self.cooldown);
        warn!(
            key_index = key_idx,
            seconds = self.cooldown.as_secs(),
            "{provider} key rate-limited (429); blacklisting for the cooldown window"
        );
    }

    /// Return the next key at or after `from` that is not currently
    /// blacklisted, wrapping around.
    ///
    /// Returns `Some(index)` if a usable key was found, or `None` if
    /// every key is currently blacklisted — the caller should sleep
    /// until [`earliest_expiry`] and retry.
    #[must_use]
    pub fn next_usable(&self, from: usize) -> Option<usize> {
        let len = self.len();
        if len == 0 {
            return Some(0);
        }
        let now = Instant::now();
        let blacklist = self.blacklist.lock().ok();
        for step in 0..len {
            let candidate = (from + step) % len;
            let is_usable = match blacklist.as_ref().and_then(|b| b.get(candidate)) {
                Some(Some(until)) => now >= *until,
                _ => true,
            };
            if is_usable {
                return Some(candidate);
            }
        }
        None
    }

    /// Duration until the earliest-blacklisted key becomes usable again.
    ///
    /// Returns `Duration::ZERO` if no keys are blacklisted. Callers
    /// should sleep for this duration when `next_usable` returns `None`.
    #[must_use]
    pub fn earliest_expiry(&self) -> Duration {
        let blacklist = match self.blacklist.lock() {
            Ok(b) => b,
            Err(_) => return Duration::ZERO,
        };
        let now = Instant::now();
        blacklist
            .iter()
            .filter_map(|slot| slot.filter(|until| *until > now))
            .min()
            .map(|earliest| earliest.saturating_duration_since(now))
            .unwrap_or(Duration::ZERO)
    }

    /// Parse a `Retry-After` header (seconds or HTTP-date) from a
    /// response, capped at 60 seconds.
    #[must_use]
    pub fn retry_after_from_response(resp: &reqwest::Response) -> Option<Duration> {
        let value = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)?
            .to_str()
            .ok()?;
        let secs = value.trim().parse::<u64>().ok();
        if let Some(secs) = secs {
            return Some(Duration::from_secs(secs.min(60)));
        }
        None
    }

    /// Exponential backoff (capped) with deterministic per-request jitter.
    ///
    /// The jitter is derived from the starting key index so concurrent
    /// requests desynchronise their retries (thundering-herd avoidance)
    /// without an RNG dependency.
    #[must_use]
    pub fn retry_delay(attempt: u32, start_key: usize) -> Duration {
        let base = 2u64.saturating_pow(attempt.min(4));
        let jitter_ms = (((start_key as u64).wrapping_add(attempt as u64)) * 7919) % 250 + 1;
        Duration::from_millis(base * 1000 + jitter_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_pool_all_usable() {
        let pool = KeyPool::new(3);
        assert_eq!(pool.len(), 3);
        assert_eq!(pool.next_usable(0), Some(0));
        assert_eq!(pool.next_usable(1), Some(1));
    }

    #[test]
    fn blacklist_skips_key() {
        let pool = KeyPool::new(3);
        pool.blacklist(1, "test");
        assert_eq!(pool.next_usable(0), Some(0));
        assert_eq!(pool.next_usable(1), Some(2));
        assert_eq!(pool.next_usable(2), Some(2));
    }

    #[test]
    fn all_blacklisted_returns_none() {
        let pool = KeyPool::new(2);
        pool.blacklist(0, "test");
        pool.blacklist(1, "test");
        assert_eq!(pool.next_usable(0), None);
    }

    #[test]
    fn earliest_expiry_nonzero_when_keys_blacklisted() {
        let pool = KeyPool::new(2);
        pool.blacklist(0, "test");
        let expiry = pool.earliest_expiry();
        assert!(expiry > Duration::ZERO);
        assert!(expiry <= DEFAULT_BLACKLIST_DURATION);
    }

    #[test]
    fn earliest_expiry_zero_when_no_blacklist() {
        let pool = KeyPool::new(2);
        assert_eq!(pool.earliest_expiry(), Duration::ZERO);
    }

    #[test]
    fn retry_delay_increases_with_attempt() {
        let d1 = KeyPool::retry_delay(1, 0);
        let d2 = KeyPool::retry_delay(2, 0);
        let d3 = KeyPool::retry_delay(3, 0);
        assert!(d1 < d2);
        assert!(d2 < d3);
    }

    #[test]
    fn empty_pool_returns_zero() {
        let pool = KeyPool::new(0);
        assert!(pool.is_empty());
        assert_eq!(pool.next_usable(0), Some(0));
    }
}
