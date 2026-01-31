//! Linux Capabilities Demo
//!
//! This demo shows how capabilities work and how we drop them.
//!
//! Run with: cargo run --example capabilities_demo
//!
//! NOTE: Must run as root to see full capabilities:
//!   sudo -E cargo run --example capabilities_demo

use neurovisor::security::{CapabilityDropper, get_current_caps};

fn main() {
    println!("┌─────────────────────────────────────────┐");
    println!("│  Linux Capabilities Demo                │");
    println!("└─────────────────────────────────────────┘\n");

    // ─────────────────────────────────────────────────────────────────────
    // Step 1: Show current capabilities
    // ─────────────────────────────────────────────────────────────────────
    println!("1. Current capabilities of this process:\n");
    match get_current_caps() {
        Ok(caps) => println!("{}", caps),
        Err(e) => println!("   Unable to read caps: {}\n", e),
    }

    // ─────────────────────────────────────────────────────────────────────
    // Step 2: Show what we would drop
    // ─────────────────────────────────────────────────────────────────────
    println!("2. Capabilities that would be DROPPED for Firecracker:\n");
    let dropper = CapabilityDropper::with_firecracker_drops();
    for (i, cap) in dropper.caps_being_dropped().iter().enumerate() {
        if i < 10 {
            println!("   {} {}", "❌", cap);
        }
    }
    if dropper.drop_count() > 10 {
        println!("   ... and {} more\n", dropper.drop_count() - 10);
    } else {
        println!();
    }
    println!("   Total: {} capabilities to drop\n", dropper.drop_count());

    // ─────────────────────────────────────────────────────────────────────
    // Step 3: Explain why we keep certain caps
    // ─────────────────────────────────────────────────────────────────────
    println!("3. Capabilities we KEEP (Firecracker needs these):\n");
    println!("   {} CAP_DAC_OVERRIDE  - access /dev/kvm without file perms", "✅");
    println!("   {} CAP_SYS_RESOURCE  - set memory limits for VM", "✅");
    println!();

    // ─────────────────────────────────────────────────────────────────────
    // Step 4: Actually drop capabilities (if running as root)
    // ─────────────────────────────────────────────────────────────────────
    println!("4. Applying capability drops...\n");

    // Only drop a few safe ones for the demo (not all Firecracker drops)
    let mut demo_dropper = CapabilityDropper::new();
    demo_dropper
        .drop_cap(caps::Capability::CAP_SYSLOG)     // Safe to drop
        .drop_cap(caps::Capability::CAP_WAKE_ALARM); // Safe to drop

    match demo_dropper.apply() {
        Ok(()) => {
            println!("   {} Dropped {} capabilities", "✅", demo_dropper.drop_count());
            println!();
            println!("5. Capabilities AFTER dropping:\n");
            if let Ok(caps) = get_current_caps() {
                println!("{}", caps);
            }
        }
        Err(e) => {
            println!("   {} Failed to drop capabilities: {}", "⚠️ ", e);
            println!("   (This is normal - need root or CAP_SETPCAP to drop caps)\n");
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Step 5: Explain security architecture
    // ─────────────────────────────────────────────────────────────────────
    println!("┌─────────────────────────────────────────────────────────┐");
    println!("│  HOW THIS WORKS IN PRODUCTION:                         │");
    println!("│                                                         │");
    println!("│  1. NeuroVisor runs as root (needs /dev/kvm)           │");
    println!("│  2. Fork child process for Firecracker                 │");
    println!("│  3. Child drops dangerous capabilities                 │");
    println!("│  4. Child applies seccomp filter                       │");
    println!("│  5. Child execs Firecracker (inherits restrictions)    │");
    println!("│                                                         │");
    println!("│  Result: Firecracker runs with minimal privileges!     │");
    println!("└─────────────────────────────────────────────────────────┘");
}
