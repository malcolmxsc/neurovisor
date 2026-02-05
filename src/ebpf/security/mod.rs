//! eBPF-based security enforcement
//!
//! This module provides LSM (Linux Security Module) hooks via eBPF for
//! fine-grained access control on Firecracker VM processes.
//!
//! ## Features
//!
//! - **File access control**: Block access to sensitive paths
//! - **Policy-based**: Configurable blocked paths
//! - **Per-PID tracking**: Only enforces on registered VM processes
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │  Kernel Space (LSM BPF)                                     │
//! │                                                             │
//! │  lsm/file_open hook                                        │
//! │         │                                                   │
//! │         ▼                                                   │
//! │  ┌─────────────────┐    ┌──────────────────┐               │
//! │  │ file_open_check │───►│  TRACKED_PIDS    │               │
//! │  │   LSM program   │    │    BPF Map       │               │
//! │  └────────┬────────┘    └──────────────────┘               │
//! │           │                                                 │
//! │           ▼                                                 │
//! │  ┌──────────────────┐                                       │
//! │  │  BLOCKED_PATHS   │──► Return -EACCES if blocked         │
//! │  │    BPF Map       │                                       │
//! │  └──────────────────┘                                       │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Defense-in-Depth
//!
//! LSM BPF provides context-aware enforcement that complements seccomp:
//!
//! - **Seccomp**: Binary allow/kill per syscall (no arguments)
//! - **LSM BPF**: Path-based decisions (allow `/tmp/*`, deny `/etc/shadow`)

mod lsm;
mod policy;

pub use lsm::{LsmError, LsmManager};
pub use policy::{SecurityPolicy, DEFAULT_BLOCKED_PATHS};
