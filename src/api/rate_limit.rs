//! Per-client token bucket for the public `/query` endpoint.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::ip_in_cidr;

/// Bounds memory when many distinct clients appear (a botnet, or one host
/// rotating source addresses).
const MAX_TRACKED_CLIENTS: usize = 100_000;

pub struct RateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    exempt: Vec<(IpAddr, u8)>,
    max_tracked: usize,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

struct Bucket {
    tokens: f64,
    updated: Instant,
}

impl RateLimiter {
    /// `burst` 0 means the same as `per_minute`. `per_minute` must be non-zero.
    pub fn new(per_minute: u32, burst: u32, exempt: Vec<(IpAddr, u8)>) -> Self {
        Self::with_max_tracked(per_minute, burst, exempt, MAX_TRACKED_CLIENTS)
    }

    fn with_max_tracked(
        per_minute: u32,
        burst: u32,
        exempt: Vec<(IpAddr, u8)>,
        max_tracked: usize,
    ) -> Self {
        let burst = if burst == 0 { per_minute } else { burst };
        Self {
            capacity: f64::from(burst),
            refill_per_sec: f64::from(per_minute) / 60.0,
            exempt,
            max_tracked,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// `Ok` when the client may proceed; `Err(retry_after)` when it is over
    /// its rate.
    pub fn check(&self, client: IpAddr, now: Instant) -> Result<(), Duration> {
        if self
            .exempt
            .iter()
            .any(|(net, prefix)| ip_in_cidr(&client, net, *prefix))
        {
            return Ok(());
        }
        let key = bucket_key(client);
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if !buckets.contains_key(&key) && buckets.len() >= self.max_tracked {
            // A bucket that has refilled completely carries no state.
            let refill_time = Duration::from_secs_f64(self.capacity / self.refill_per_sec);
            buckets.retain(|_, b| now.saturating_duration_since(b.updated) < refill_time);
            if buckets.len() >= self.max_tracked {
                // Too many clients mid-burst to track one more; a per-client
                // limit cannot stop a distributed flood anyway, so let it pass.
                return Ok(());
            }
        }
        let bucket = buckets.entry(key).or_insert(Bucket {
            tokens: self.capacity,
            updated: now,
        });
        let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.updated = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64(
                (1.0 - bucket.tokens) / self.refill_per_sec,
            ))
        }
    }
}

/// IPv6 clients are keyed by their /64: one subscriber usually owns a whole
/// /64 and could otherwise take a fresh bucket per address.
fn bucket_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !((1u128 << 64) - 1))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn allows_the_burst_then_rejects_with_a_retry_hint() {
        let rl = RateLimiter::new(60, 3, vec![]);
        let t = Instant::now();
        for _ in 0..3 {
            assert!(rl.check(ip("203.0.113.7"), t).is_ok());
        }
        let retry = rl.check(ip("203.0.113.7"), t).unwrap_err();
        assert!(
            retry > Duration::from_millis(900) && retry <= Duration::from_secs(1),
            "{retry:?}"
        );
    }

    #[test]
    fn refills_at_the_per_minute_rate() {
        let rl = RateLimiter::new(60, 1, vec![]);
        let t = Instant::now();
        assert!(rl.check(ip("203.0.113.7"), t).is_ok());
        assert!(
            rl.check(ip("203.0.113.7"), t + Duration::from_millis(500))
                .is_err()
        );
        assert!(
            rl.check(ip("203.0.113.7"), t + Duration::from_millis(1100))
                .is_ok()
        );
    }

    #[test]
    fn refill_never_exceeds_the_burst() {
        let rl = RateLimiter::new(60, 2, vec![]);
        let t = Instant::now();
        let later = t + Duration::from_secs(3600);
        assert!(rl.check(ip("203.0.113.7"), later).is_ok());
        assert!(rl.check(ip("203.0.113.7"), later).is_ok());
        assert!(rl.check(ip("203.0.113.7"), later).is_err());
    }

    #[test]
    fn burst_zero_means_per_minute() {
        let rl = RateLimiter::new(5, 0, vec![]);
        let t = Instant::now();
        for _ in 0..5 {
            assert!(rl.check(ip("203.0.113.7"), t).is_ok());
        }
        assert!(rl.check(ip("203.0.113.7"), t).is_err());
    }

    #[test]
    fn clients_do_not_share_buckets() {
        let rl = RateLimiter::new(60, 1, vec![]);
        let t = Instant::now();
        assert!(rl.check(ip("203.0.113.7"), t).is_ok());
        assert!(rl.check(ip("203.0.113.7"), t).is_err());
        assert!(rl.check(ip("198.51.100.9"), t).is_ok());
    }

    #[test]
    fn exempt_cidr_is_never_limited() {
        let rl = RateLimiter::new(60, 1, vec![(ip("159.195.144.236"), 32)]);
        let t = Instant::now();
        for _ in 0..100 {
            assert!(rl.check(ip("159.195.144.236"), t).is_ok());
        }
    }

    #[test]
    fn ipv6_addresses_in_one_slash_64_share_a_bucket() {
        let rl = RateLimiter::new(60, 1, vec![]);
        let t = Instant::now();
        assert!(rl.check(ip("2001:db8:1:2::1"), t).is_ok());
        assert!(rl.check(ip("2001:db8:1:2:ffff::9"), t).is_err());
        assert!(rl.check(ip("2001:db8:1:3::1"), t).is_ok());
    }

    #[test]
    fn ipv4_mapped_ipv6_shares_the_ipv4_bucket() {
        let rl = RateLimiter::new(60, 1, vec![]);
        let t = Instant::now();
        assert!(rl.check(ip("203.0.113.7"), t).is_ok());
        assert!(rl.check(ip("::ffff:203.0.113.7"), t).is_err());
    }

    #[test]
    fn memory_stays_bounded_and_idle_buckets_are_dropped() {
        let rl = RateLimiter::with_max_tracked(60, 1, vec![], 3);
        let t = Instant::now();
        for i in 1..=3 {
            assert!(rl.check(ip(&format!("203.0.113.{i}")), t).is_ok());
        }
        // Full and all mid-burst: a 4th client passes untracked.
        assert!(rl.check(ip("203.0.113.4"), t).is_ok());
        assert_eq!(rl.buckets.lock().unwrap().len(), 3);
        // Once the old buckets have refilled they are dropped to make room.
        let later = t + Duration::from_secs(2);
        assert!(rl.check(ip("203.0.113.5"), later).is_ok());
        assert_eq!(rl.buckets.lock().unwrap().len(), 1);
        assert!(rl.check(ip("203.0.113.5"), later).is_err());
    }
}
