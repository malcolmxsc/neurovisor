//! Security module for NeuroVisor sandboxing
//!
//! This module provides security primitives for isolating Firecracker VMs:
//! - Seccomp BPF filters (restrict syscalls)
//! - Capability dropping (remove root powers)
//!
//! # Security Layers
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │  Layer 1: CAPABILITIES                                          │
//! │  Drop dangerous root powers BEFORE starting Firecracker         │
//! │                                                                 │
//! │  ┌──────────────────────────────────────────────────────────┐  │
//! │  │ DROP: CAP_SYS_ADMIN, CAP_PTRACE, CAP_NET_RAW, etc.      │  │
//! │  │ KEEP: CAP_DAC_OVERRIDE (for /dev/kvm access)            │  │
//! │  └──────────────────────────────────────────────────────────┘  │
//! └─────────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────────┐
//! │  Layer 2: SECCOMP BPF                                           │
//! │  Block dangerous syscalls at kernel level                       │
//! │                                                                 │
//! │  Process ──syscall──► Filter ──allowed?──► Kernel               │
//! │                          │                                      │
//! │                          └─blocked─► SIGKILL (process dies)    │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage Order
//!
//! Apply security in this order (in forked child, before exec):
//! 1. Drop capabilities (CapabilityDropper::apply)
//! 2. Apply seccomp filter (FirecrackerSeccomp::apply)
//! 3. exec(firecracker)

pub mod seccomp;
pub mod capabilities;
pub mod rate_limit;

pub use seccomp::FirecrackerSeccomp;
pub use capabilities::{CapabilityDropper, get_current_caps};
pub use rate_limit::{RateLimiter, RateLimitError};
