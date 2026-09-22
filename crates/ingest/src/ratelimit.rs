//! Token-bucket rate limiter, shared across tasks via `Arc`.
//!
//! Not RPC-specific: anything that must respect a requests-per-second budget
//! can `acquire()` before acting and `penalize()` when the far side pushes
//! back (HTTP 429 / `Retry-After`).

use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitConfig {
    /// Sustained budget.
    pub requests_per_second: u32,
    /// Maximum tokens banked while idle.
    pub burst: u32,
}

impl Default for RateLimitConfig {
    /// Conservative enough for the public mainnet endpoint.
    fn default() -> Self {
        Self {
            requests_per_second: 4,
            burst: 8,
        }
    }
}

#[derive(Debug)]
struct State {
    /// tokens × 1_000_000, to refill in integer arithmetic
    micro_tokens: u64,
    last_refill: Instant,
    /// set by `penalize`; no tokens are handed out before this instant
    paused_until: Option<Instant>,
}

#[derive(Debug)]
pub struct RateLimiter {
    cfg: RateLimitConfig,
    state: Mutex<State>,
}

const MICRO: u64 = 1_000_000;

impl RateLimiter {
    pub fn new(cfg: RateLimitConfig) -> Self {
        let cfg = RateLimitConfig {
            requests_per_second: cfg.requests_per_second.max(1),
            burst: cfg.burst.max(1),
        };
        Self {
            cfg,
            state: Mutex::new(State {
                micro_tokens: cfg.burst as u64 * MICRO,
                last_refill: Instant::now(),
                paused_until: None,
            }),
        }
    }

    pub fn config(&self) -> RateLimitConfig {
        self.cfg
    }

    /// Wait until one request may be made.
    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut st = self.state.lock().await;
                let now = Instant::now();
                if let Some(until) = st.paused_until {
                    if now < until {
                        Some(until - now)
                    } else {
                        st.paused_until = None;
                        st.last_refill = now;
                        None
                    }
                } else {
                    None
                }
                .or_else(|| {
                    self.refill(&mut st, now);
                    if st.micro_tokens >= MICRO {
                        st.micro_tokens -= MICRO;
                        None
                    } else {
                        let deficit = MICRO - st.micro_tokens;
                        // time to earn `deficit` micro-tokens at rps
                        let nanos = deficit * 1_000_000_000 / (self.cfg.requests_per_second as u64 * MICRO);
                        Some(Duration::from_nanos(nanos.max(1)))
                    }
                })
            };
            match wait {
                None => return,
                Some(d) => tokio::time::sleep(d).await,
            }
        }
    }

    /// Take a token only if one is available right now.
    pub async fn try_acquire(&self) -> bool {
        let mut st = self.state.lock().await;
        let now = Instant::now();
        if let Some(until) = st.paused_until {
            if now < until {
                return false;
            }
            st.paused_until = None;
            st.last_refill = now;
        }
        self.refill(&mut st, now);
        if st.micro_tokens >= MICRO {
            st.micro_tokens -= MICRO;
            true
        } else {
            false
        }
    }

    /// The far side asked us to back off: drain the bucket and pause.
    pub async fn penalize(&self, pause: Duration) {
        let mut st = self.state.lock().await;
        st.micro_tokens = 0;
        let until = Instant::now() + pause;
        st.paused_until = Some(st.paused_until.map_or(until, |u| u.max(until)));
        tracing::debug!(?pause, "rate limiter penalized");
    }

    fn refill(&self, st: &mut State, now: Instant) {
        let elapsed = now.saturating_duration_since(st.last_refill);
        let cap = self.cfg.burst as u64 * MICRO;
        let earned = elapsed.as_nanos() * self.cfg.requests_per_second as u128 * MICRO as u128 / 1_000_000_000;
        let earned = earned.min(cap as u128) as u64;
        if earned > 0 {
            st.micro_tokens = st.micro_tokens.saturating_add(earned).min(cap);
            st.last_refill = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn burst_then_throttle() {
        let rl = RateLimiter::new(RateLimitConfig {
            requests_per_second: 2,
            burst: 3,
        });
        let t0 = Instant::now();
        for _ in 0..3 {
            rl.acquire().await;
        }
        assert_eq!(Instant::now(), t0, "burst should not wait");
        rl.acquire().await; // 4th needs 500ms of refill
        assert!(Instant::now() - t0 >= Duration::from_millis(500));
        rl.acquire().await;
        assert!(Instant::now() - t0 >= Duration::from_millis(1000));
    }

    #[tokio::test(start_paused = true)]
    async fn try_acquire_never_waits() {
        let rl = RateLimiter::new(RateLimitConfig {
            requests_per_second: 1,
            burst: 1,
        });
        assert!(rl.try_acquire().await);
        assert!(!rl.try_acquire().await);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(rl.try_acquire().await);
    }

    #[tokio::test(start_paused = true)]
    async fn penalize_pauses_everything() {
        let rl = RateLimiter::new(RateLimitConfig::default());
        rl.acquire().await;
        let t0 = Instant::now();
        rl.penalize(Duration::from_secs(5)).await;
        rl.acquire().await;
        assert!(Instant::now() - t0 >= Duration::from_secs(5));
    }
}
