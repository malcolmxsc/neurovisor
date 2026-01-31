//! Rate Limiting for Vsock Connections
//!
//! # Why Rate Limiting?
//!
//! A malicious or buggy guest VM could flood the host with requests,
//! causing denial of service. Rate limiting prevents this by enforcing
//! a maximum requests-per-second limit.
//!
//! # Algorithm: Token Bucket
//!
//! We use the "token bucket" algorithm - simple and effective:
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────────────┐
//! │  TOKEN BUCKET ALGORITHM                                          │
//! │                                                                   │
//! │  ┌─────────────┐                                                 │
//! │  │   Bucket    │ ← Holds tokens (max = capacity)                │
//! │  │  ● ● ● ●    │                                                 │
//! │  │  ● ● ●      │ ← Tokens refill at rate R per second           │
//! │  └─────────────┘                                                 │
//! │        │                                                         │
//! │        ▼                                                         │
//! │  Request arrives:                                                │
//! │    - If token available → consume 1 token, ALLOW request        │
//! │    - If bucket empty → DENY request (rate limited)              │
//! │                                                                   │
//! │  EXAMPLE: capacity=10, rate=5/sec                                │
//! │    - Can handle burst of 10 requests instantly                  │
//! │    - Then 5 requests/sec sustained                              │
//! └───────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Why Token Bucket?
//!
//! 1. **Allows bursts**: Guest can send N requests quickly (up to capacity)
//! 2. **Smooth average**: Over time, rate converges to refill_rate
//! 3. **Simple**: Easy to understand and implement
//! 4. **Memory efficient**: Only stores: tokens, last_refill_time

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::sync::Mutex;

// ═══════════════════════════════════════════════════════════════════════════
// RateLimiter (Token Bucket)
// ═══════════════════════════════════════════════════════════════════════════
// WHAT IT IS:
//   A thread-safe rate limiter using the token bucket algorithm.
//
// HOW TO USE:
//   1. Create with capacity (burst size) and rate (requests/sec)
//   2. Call try_acquire() before each request
//   3. If true, proceed; if false, reject or queue
//
// EXAMPLE:
//   let limiter = RateLimiter::new(10, 5.0);  // 10 burst, 5/sec
//   if limiter.try_acquire() {
//       // Handle request
//   } else {
//       // Rate limited - reject or delay
//   }
// ═══════════════════════════════════════════════════════════════════════════

pub struct RateLimiter {
    /// Maximum tokens in bucket (burst capacity)
    capacity: u64,

    /// Tokens added per second
    refill_rate: f64,

    /// Current token count (scaled by 1000 for precision)
    /// Using AtomicU64 for thread-safety without locks
    tokens_scaled: AtomicU64,

    /// Last time we refilled tokens
    /// Using Mutex because Instant isn't atomic
    last_refill: Mutex<Instant>,
}

// Scale factor for token precision
// We store tokens * 1000 to handle fractional tokens without floats
const SCALE: u64 = 1000;

impl RateLimiter {
    // ═══════════════════════════════════════════════════════════════════════
    // new(capacity, refill_rate) - Constructor
    // ═══════════════════════════════════════════════════════════════════════
    // ARGUMENTS:
    //   capacity: u64 - Maximum tokens (burst size)
    //   refill_rate: f64 - Tokens added per second
    //
    // EXAMPLE:
    //   RateLimiter::new(100, 10.0)
    //   → Allows burst of 100, then 10 requests/sec sustained
    // ═══════════════════════════════════════════════════════════════════════
    pub fn new(capacity: u64, refill_rate: f64) -> Self {
        Self {
            capacity,
            refill_rate,
            // Start with full bucket
            tokens_scaled: AtomicU64::new(capacity * SCALE),
            last_refill: Mutex::new(Instant::now()),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // with_defaults() - Reasonable defaults for inference
    // ═══════════════════════════════════════════════════════════════════════
    // Default: 50 burst, 10 requests/sec
    // Rationale: LLM inference is slow, so 10 req/sec is plenty
    // ═══════════════════════════════════════════════════════════════════════
    pub fn with_defaults() -> Self {
        Self::new(50, 10.0)
    }

    // ═══════════════════════════════════════════════════════════════════════
    // try_acquire() - Attempt to acquire a token
    // ═══════════════════════════════════════════════════════════════════════
    // WHAT IT DOES:
    //   1. Refill tokens based on elapsed time
    //   2. Try to consume one token
    //   3. Return true if token acquired, false if rate limited
    //
    // THREAD SAFETY:
    //   Safe to call from multiple threads simultaneously.
    //   Uses atomic operations for token count.
    //
    // RETURNS:
    //   true  → Request allowed (token consumed)
    //   false → Request denied (rate limited)
    // ═══════════════════════════════════════════════════════════════════════
    pub fn try_acquire(&self) -> bool {
        // Step 1: Refill tokens based on elapsed time
        self.refill();

        // Step 2: Try to consume one token atomically
        loop {
            let current = self.tokens_scaled.load(Ordering::Relaxed);

            // Check if we have at least 1 token (scaled)
            if current < SCALE {
                return false; // Rate limited
            }

            // Try to decrement by 1 token (SCALE)
            // compare_exchange ensures no race condition
            match self.tokens_scaled.compare_exchange(
                current,
                current - SCALE,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true, // Successfully consumed token
                Err(_) => continue,    // Another thread modified, retry
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // refill() - Add tokens based on elapsed time
    // ═══════════════════════════════════════════════════════════════════════
    // WHAT IT DOES:
    //   Calculates how many tokens to add since last refill.
    //   tokens_to_add = elapsed_seconds * refill_rate
    //
    // ```text
    // TIME ──────────────────────────────────────────►
    //
    //   last_refill                              now
    //       │                                     │
    //       ▼                                     ▼
    //       ├─────────elapsed (0.5 sec)──────────┤
    //
    //   If refill_rate = 10/sec:
    //       tokens_to_add = 0.5 * 10 = 5 tokens
    // ```
    // ═══════════════════════════════════════════════════════════════════════
    fn refill(&self) {
        let now = Instant::now();

        // Lock to read/update last_refill
        let mut last = self.last_refill.lock().unwrap();
        let elapsed = now.duration_since(*last);

        // Calculate tokens to add (with scaling for precision)
        let tokens_to_add = (elapsed.as_secs_f64() * self.refill_rate * SCALE as f64) as u64;

        if tokens_to_add > 0 {
            // Update last refill time
            *last = now;

            // Add tokens, but don't exceed capacity
            let max_scaled = self.capacity * SCALE;
            loop {
                let current = self.tokens_scaled.load(Ordering::Relaxed);
                let new = std::cmp::min(current + tokens_to_add, max_scaled);

                if self.tokens_scaled
                    .compare_exchange(current, new, Ordering::SeqCst, Ordering::Relaxed)
                    .is_ok()
                {
                    break;
                }
            }
        }
    }

    /// Get current token count (for monitoring)
    pub fn available_tokens(&self) -> u64 {
        self.tokens_scaled.load(Ordering::Relaxed) / SCALE
    }

    /// Get the configured capacity
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Get the configured refill rate
    pub fn refill_rate(&self) -> f64 {
        self.refill_rate
    }

    /// Check if currently rate limited (no tokens available)
    pub fn is_rate_limited(&self) -> bool {
        self.tokens_scaled.load(Ordering::Relaxed) < SCALE
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// RateLimitError
// ═══════════════════════════════════════════════════════════════════════════
// Error returned when a request is rate limited.
// Contains helpful info for the caller.
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug)]
pub struct RateLimitError {
    /// How long to wait before retrying
    pub retry_after: Duration,
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Rate limited. Retry after {:?}",
            self.retry_after
        )
    }
}

impl std::error::Error for RateLimitError {}

impl RateLimitError {
    /// Create a rate limit error with retry_after based on refill rate
    pub fn new(refill_rate: f64) -> Self {
        // Time to get 1 token = 1 / refill_rate seconds
        let retry_secs = if refill_rate > 0.0 {
            1.0 / refill_rate
        } else {
            1.0
        };
        Self {
            retry_after: Duration::from_secs_f64(retry_secs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_rate_limit() {
        let limiter = RateLimiter::new(3, 1.0); // 3 burst, 1/sec

        // Should allow first 3 (burst capacity)
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());

        // 4th should be denied
        assert!(!limiter.try_acquire());
    }

    #[test]
    fn test_refill() {
        let limiter = RateLimiter::new(2, 100.0); // 2 burst, 100/sec

        // Consume all tokens
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire());

        // Wait for refill (10ms = 1 token at 100/sec)
        std::thread::sleep(Duration::from_millis(15));

        // Should have 1 token now
        assert!(limiter.try_acquire());
    }

    #[test]
    fn test_default_config() {
        let limiter = RateLimiter::with_defaults();
        assert_eq!(limiter.capacity(), 50);
        assert!((limiter.refill_rate() - 10.0).abs() < 0.01);
    }

    #[test]
    fn test_available_tokens() {
        let limiter = RateLimiter::new(5, 1.0);
        assert_eq!(limiter.available_tokens(), 5);

        limiter.try_acquire();
        assert_eq!(limiter.available_tokens(), 4);
    }
}
