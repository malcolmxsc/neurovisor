//! Prometheus metrics for eBPF observability
//!
//! These metrics are populated from eBPF map data collected by the EbpfManager.

use lazy_static::lazy_static;
use prometheus::{register_counter_vec, register_gauge, CounterVec, Gauge};

lazy_static! {
    // ─────────────────────────────────────────────────────────────────────────────
    // Syscall Tracing Metrics
    // ─────────────────────────────────────────────────────────────────────────────

    /// Total syscalls traced via eBPF, by VM and syscall name.
    ///
    /// This counter is incremented when collect_metrics() reads from the
    /// eBPF SYSCALL_COUNTS map.
    ///
    /// Labels:
    /// - vm_id: Hash of the VM ID (16-char hex string)
    /// - syscall: Name of the syscall (e.g., "read", "write", "openat")
    pub static ref EBPF_SYSCALL_COUNT: CounterVec = register_counter_vec!(
        "neurovisor_ebpf_syscall_total",
        "Total syscalls traced via eBPF per VM",
        &["vm_id", "syscall"]
    ).expect("failed to register EBPF_SYSCALL_COUNT metric");

    /// Process executions traced inside VMs.
    ///
    /// Incremented when execve() syscalls are detected for tracked PIDs.
    ///
    /// Labels:
    /// - vm_id: Hash of the VM ID
    /// - command: The command being executed (from comm)
    pub static ref EBPF_PROCESS_EXEC: CounterVec = register_counter_vec!(
        "neurovisor_ebpf_process_exec_total",
        "Process executions traced via eBPF inside VMs",
        &["vm_id", "command"]
    ).expect("failed to register EBPF_PROCESS_EXEC metric");

    // ─────────────────────────────────────────────────────────────────────────────
    // LSM Security Metrics
    // ─────────────────────────────────────────────────────────────────────────────

    /// Blocked file access attempts per path.
    ///
    /// Incremented when LSM denies access to a blocked path.
    ///
    /// Labels:
    /// - path: The blocked path prefix (e.g., "/etc/shadow", "/proc/kcore")
    pub static ref EBPF_LSM_BLOCKED: CounterVec = register_counter_vec!(
        "neurovisor_ebpf_lsm_blocked_total",
        "File access attempts blocked by eBPF LSM per path",
        &["path"]
    ).expect("failed to register EBPF_LSM_BLOCKED metric");

    /// Total blocked access attempts (all paths combined).
    pub static ref EBPF_LSM_BLOCKED_TOTAL: Gauge = register_gauge!(
        "neurovisor_ebpf_lsm_blocked_total_count",
        "Total file access attempts blocked by eBPF LSM"
    ).expect("failed to register EBPF_LSM_BLOCKED_TOTAL metric");

    // ─────────────────────────────────────────────────────────────────────────────
    // eBPF System Metrics
    // ─────────────────────────────────────────────────────────────────────────────

    /// Number of PIDs currently being traced.
    ///
    /// This represents the number of Firecracker processes with active eBPF tracing.
    pub static ref EBPF_TRACKED_PIDS: Gauge = register_gauge!(
        "neurovisor_ebpf_tracked_pids",
        "Number of PIDs currently being traced via eBPF"
    ).expect("failed to register EBPF_TRACKED_PIDS metric");

    /// Whether eBPF tracing is enabled (1) or disabled (0).
    ///
    /// This indicates if the kernel supports eBPF and programs loaded successfully.
    pub static ref EBPF_ENABLED: Gauge = register_gauge!(
        "neurovisor_ebpf_enabled",
        "Whether eBPF tracing is enabled (1) or disabled (0)"
    ).expect("failed to register EBPF_ENABLED metric");
}

/// Initialize eBPF metrics with default values.
///
/// Called during startup to ensure metrics exist in the registry.
pub fn init() {
    // Touch metrics to ensure they're registered
    let _ = EBPF_ENABLED.set(0.0);
    let _ = EBPF_TRACKED_PIDS.set(0.0);
}

/// Mark eBPF as enabled in metrics.
pub fn set_enabled(enabled: bool) {
    EBPF_ENABLED.set(if enabled { 1.0 } else { 0.0 });
}

/// Update the count of tracked PIDs.
pub fn set_tracked_pids(count: usize) {
    EBPF_TRACKED_PIDS.set(count as f64);
}
