//! Rate Limiting Demo
//!
//! Demonstrates the token bucket rate limiter for vsock protection.
//!
//! Run with: cargo run --example rate_limit_demo

use neurovisor::security::RateLimiter;
use std::time::{Duration, Instant};

fn main() {
    println!("┌─────────────────────────────────────────┐");
    println!("│  Token Bucket Rate Limiter Demo         │");
    println!("└─────────────────────────────────────────┘\n");

    // ─────────────────────────────────────────────────────────────────────
    // Demo 1: Burst capacity
    // ─────────────────────────────────────────────────────────────────────
    println!("1. Burst Capacity Test");
    println!("   Config: capacity=5, rate=2/sec\n");

    let limiter = RateLimiter::new(5, 2.0);

    println!("   Sending 7 requests instantly:");
    for i in 1..=7 {
        let allowed = limiter.try_acquire();
        let status = if allowed { "ALLOWED" } else { "DENIED " };
        let emoji = if allowed { "✅" } else { "❌" };
        println!("   Request {}: {} {}", i, emoji, status);
    }
    println!();

    // ─────────────────────────────────────────────────────────────────────
    // Demo 2: Token refill over time
    // ─────────────────────────────────────────────────────────────────────
    println!("2. Token Refill Test");
    println!("   Waiting for tokens to refill...\n");

    // Wait 1 second (should get 2 tokens at 2/sec)
    std::thread::sleep(Duration::from_secs(1));

    println!("   After 1 second wait (expect ~2 tokens):");
    println!("   Available tokens: {}", limiter.available_tokens());

    let allowed_count = (0..5).filter(|_| limiter.try_acquire()).count();
    println!("   Requests allowed: {}\n", allowed_count);

    // ─────────────────────────────────────────────────────────────────────
    // Demo 3: Sustained rate
    // ─────────────────────────────────────────────────────────────────────
    println!("3. Sustained Rate Test");
    println!("   Config: capacity=10, rate=20/sec");
    println!("   Sending requests for 500ms with 25ms spacing (40/sec attempted)\n");

    let limiter2 = RateLimiter::new(10, 20.0);
    let start = Instant::now();
    let mut allowed = 0;
    let mut denied = 0;

    while start.elapsed() < Duration::from_millis(500) {
        if limiter2.try_acquire() {
            allowed += 1;
        } else {
            denied += 1;
        }
        std::thread::sleep(Duration::from_millis(25)); // 40 req/sec
    }

    println!("   Requests allowed: {} (burst + sustained)", allowed);
    println!("   Requests denied:  {} (rate limited)", denied);
    println!("   Effective rate: {:.1} req/sec\n", allowed as f64 / 0.5);

    // ─────────────────────────────────────────────────────────────────────
    // Demo 4: Show how this protects NeuroVisor
    // ─────────────────────────────────────────────────────────────────────
    println!("┌─────────────────────────────────────────────────────────┐");
    println!("│  HOW THIS PROTECTS NEUROVISOR:                         │");
    println!("│                                                         │");
    println!("│  ┌────────┐                        ┌────────────────┐  │");
    println!("│  │ Guest  │ ──vsock requests──►   │  Rate Limiter  │  │");
    println!("│  │  VM    │                        │  (host-side)   │  │");
    println!("│  └────────┘                        └───────┬────────┘  │");
    println!("│                                            │           │");
    println!("│                           ┌────────────────┴───────┐   │");
    println!("│                           │                        │   │");
    println!("│                           ▼                        ▼   │");
    println!("│                    ┌────────────┐          ┌──────────┐│");
    println!("│                    │  ALLOWED   │          │  DENIED  ││");
    println!("│                    │ → process  │          │ → reject ││");
    println!("│                    └────────────┘          └──────────┘│");
    println!("│                                                         │");
    println!("│  - Prevents DoS from malicious/buggy guests            │");
    println!("│  - Allows legitimate bursts (capacity)                 │");
    println!("│  - Smooths traffic to sustainable rate                 │");
    println!("└─────────────────────────────────────────────────────────┘");
}
