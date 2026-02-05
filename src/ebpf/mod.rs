//! eBPF-based observability and security for NeuroVisor
//!
//! This module provides kernel-level tracing and security enforcement using eBPF:
//!
//! - **Observability**: Syscall tracing via tracepoints
//! - **Security**: File access control via LSM hooks
//!
//! It integrates with the existing Prometheus metrics infrastructure to
//! expose syscall counts, process executions, and file access patterns.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │  Kernel Space (eBPF programs)                               │
//! │                                                             │
//! │  tracepoint/syscalls/sys_enter ──► SYSCALL_COUNTS map      │
//! │                                                             │
//! │  PID_TO_VM map ◄── registered by userspace                  │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │  User Space (EbpfManager)                                   │
//! │                                                             │
//! │  start_tracing(vm_id, pid) → insert into PID_TO_VM         │
//! │  collect_metrics()         → read SYSCALL_COUNTS           │
//! │  stop_tracing(pid)         → remove from PID_TO_VM         │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │  Prometheus Metrics                                         │
//! │                                                             │
//! │  neurovisor_ebpf_syscall_total{vm_id, syscall}             │
//! │  neurovisor_ebpf_process_exec_total{vm_id, command}        │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! // Initialize eBPF manager (returns None if not supported)
//! let ebpf = EbpfManager::new();
//!
//! // Start tracing when VM is created
//! if let Some(ref manager) = ebpf {
//!     manager.start_tracing(&vm_id, firecracker_pid).await?;
//! }
//!
//! // Periodically collect metrics
//! if let Some(ref manager) = ebpf {
//!     manager.collect_metrics().await?;
//! }
//!
//! // Stop tracing when VM is destroyed
//! if let Some(ref manager) = ebpf {
//!     manager.stop_tracing(firecracker_pid).await?;
//! }
//! ```
//!
//! ## Requirements
//!
//! - Linux kernel 5.8+ with CONFIG_BPF_SYSCALL=y
//! - CAP_BPF and CAP_PERFMON capabilities (or root)
//! - Pre-built eBPF programs in target/ebpf/ (run build-ebpf.sh)
//! - Compiled with `--features ebpf`

#[cfg(feature = "ebpf")]
mod loader;
pub mod metrics;
pub mod security;
pub mod tracing;

#[cfg(feature = "ebpf")]
pub use loader::{EbpfError, EbpfManager};
pub use security::{LsmManager, SecurityPolicy};
pub use tracing::TraceManager;

/// Stub EbpfManager for when eBPF feature is disabled.
///
/// This allows the rest of the codebase to compile without feature flags everywhere.
#[cfg(not(feature = "ebpf"))]
pub struct EbpfManager;

#[cfg(not(feature = "ebpf"))]
impl EbpfManager {
    /// Returns None when eBPF feature is disabled.
    pub fn new() -> Option<Self> {
        println!("[EBPF] eBPF feature not enabled at compile time");
        None
    }

    pub fn is_enabled(&self) -> bool {
        false
    }

    pub async fn start_tracing(&self, _vm_id: &str, _pid: u32) -> Result<(), EbpfError> {
        Ok(())
    }

    pub async fn stop_tracing(&self, _pid: u32) -> Result<(), EbpfError> {
        Ok(())
    }

    pub async fn collect_metrics(&self) -> Result<(), EbpfError> {
        Ok(())
    }
}

/// Stub error type for when eBPF feature is disabled.
#[cfg(not(feature = "ebpf"))]
#[derive(Debug)]
pub struct EbpfError;

#[cfg(not(feature = "ebpf"))]
impl std::fmt::Display for EbpfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "eBPF not enabled")
    }
}

#[cfg(not(feature = "ebpf"))]
impl std::error::Error for EbpfError {}
