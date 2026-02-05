//! eBPF program loader and manager for NeuroVisor
//!
//! This module provides the `EbpfManager` which loads eBPF programs into the kernel
//! and manages the mapping between Firecracker PIDs and VM IDs for syscall tracing.

use aya::maps::HashMap;
use aya::programs::TracePoint;
use aya::{include_bytes_aligned, Bpf};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tokio::sync::RwLock;

use super::metrics;

/// Manages eBPF programs for VM syscall tracing.
///
/// The manager loads eBPF tracepoints that count syscalls per tracked PID,
/// allowing us to correlate syscall activity with specific VMs.
pub struct EbpfManager {
    bpf: Arc<RwLock<Bpf>>,
    enabled: bool,
}

/// Error type for eBPF operations
#[derive(Debug)]
pub enum EbpfError {
    /// Failed to load eBPF program
    LoadError(String),
    /// Failed to attach eBPF program
    AttachError(String),
    /// Failed to access BPF map
    MapError(String),
    /// eBPF is not enabled/available
    NotEnabled,
}

impl std::fmt::Display for EbpfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EbpfError::LoadError(msg) => write!(f, "eBPF load error: {}", msg),
            EbpfError::AttachError(msg) => write!(f, "eBPF attach error: {}", msg),
            EbpfError::MapError(msg) => write!(f, "eBPF map error: {}", msg),
            EbpfError::NotEnabled => write!(f, "eBPF is not enabled"),
        }
    }
}

impl std::error::Error for EbpfError {}

impl EbpfManager {
    /// Create a new eBPF manager and load the syscall tracing program.
    ///
    /// This requires:
    /// - Kernel with CONFIG_BPF_SYSCALL=y
    /// - CAP_BPF and CAP_PERFMON capabilities (or root)
    /// - Pre-built eBPF object files in target/ebpf/
    ///
    /// Returns `None` if eBPF programs cannot be loaded (graceful degradation).
    pub fn new() -> Option<Self> {
        match Self::try_new() {
            Ok(manager) => {
                println!("[EBPF] Syscall tracing enabled");
                Some(manager)
            }
            Err(e) => {
                println!("[EBPF] Failed to initialize: {} (continuing without eBPF)", e);
                None
            }
        }
    }

    fn try_new() -> Result<Self, EbpfError> {
        // Load the pre-compiled eBPF bytecode
        // This is embedded at compile time from target/ebpf/syscall-trace.o
        #[cfg(feature = "ebpf")]
        let bpf_bytes = include_bytes_aligned!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/target/ebpf/syscall-trace.o"
        ));

        #[cfg(not(feature = "ebpf"))]
        return Err(EbpfError::NotEnabled);

        #[cfg(feature = "ebpf")]
        {
            let mut bpf =
                Bpf::load(bpf_bytes).map_err(|e| EbpfError::LoadError(e.to_string()))?;

            // Attach to the syscall tracepoint
            let program: &mut TracePoint = bpf
                .program_mut("sys_enter")
                .ok_or_else(|| EbpfError::LoadError("sys_enter program not found".to_string()))?
                .try_into()
                .map_err(|e: aya::programs::ProgramError| {
                    EbpfError::LoadError(e.to_string())
                })?;

            program
                .load()
                .map_err(|e| EbpfError::LoadError(e.to_string()))?;

            program
                .attach("syscalls", "sys_enter")
                .map_err(|e| EbpfError::AttachError(e.to_string()))?;

            Ok(Self {
                bpf: Arc::new(RwLock::new(bpf)),
                enabled: true,
            })
        }
    }

    /// Check if eBPF tracing is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Start tracing syscalls for a VM.
    ///
    /// Registers the Firecracker process PID with its VM ID so that
    /// syscalls from this process (and its children) are counted.
    pub async fn start_tracing(&self, vm_id: &str, firecracker_pid: u32) -> Result<(), EbpfError> {
        if !self.enabled {
            return Ok(());
        }

        let vm_hash = hash_vm_id(vm_id);

        let mut bpf = self.bpf.write().await;
        let mut pid_map: HashMap<_, u32, u64> = bpf
            .map_mut("PID_TO_VM")
            .ok_or_else(|| EbpfError::MapError("PID_TO_VM map not found".to_string()))?
            .try_into()
            .map_err(|e: aya::maps::MapError| EbpfError::MapError(e.to_string()))?;

        pid_map
            .insert(firecracker_pid, vm_hash, 0)
            .map_err(|e| EbpfError::MapError(e.to_string()))?;

        println!(
            "[EBPF] Started tracing VM {} (PID {}, hash {})",
            vm_id, firecracker_pid, vm_hash
        );

        Ok(())
    }

    /// Stop tracing syscalls for a VM.
    ///
    /// Removes the PID from the tracking map. Called when VM is destroyed.
    pub async fn stop_tracing(&self, firecracker_pid: u32) -> Result<(), EbpfError> {
        if !self.enabled {
            return Ok(());
        }

        let mut bpf = self.bpf.write().await;
        let mut pid_map: HashMap<_, u32, u64> = bpf
            .map_mut("PID_TO_VM")
            .ok_or_else(|| EbpfError::MapError("PID_TO_VM map not found".to_string()))?
            .try_into()
            .map_err(|e: aya::maps::MapError| EbpfError::MapError(e.to_string()))?;

        let _ = pid_map.remove(&firecracker_pid); // Ignore if not found

        println!("[EBPF] Stopped tracing PID {}", firecracker_pid);

        Ok(())
    }

    /// Collect syscall counts from eBPF maps and export to Prometheus metrics.
    ///
    /// This should be called periodically (e.g., every 10 seconds) to update
    /// the Prometheus metrics with the latest syscall counts.
    pub async fn collect_metrics(&self) -> Result<(), EbpfError> {
        if !self.enabled {
            return Ok(());
        }

        let bpf = self.bpf.read().await;

        // Read syscall counts map
        let counts: HashMap<_, SyscallKey, u64> = bpf
            .map("SYSCALL_COUNTS")
            .ok_or_else(|| EbpfError::MapError("SYSCALL_COUNTS map not found".to_string()))?
            .try_into()
            .map_err(|e: aya::maps::MapError| EbpfError::MapError(e.to_string()))?;

        for item in counts.iter() {
            if let Ok((key, count)) = item {
                let vm_hash_str = format!("{:016x}", key.vm_id);
                let syscall_name = syscall_name(key.syscall_nr);

                metrics::EBPF_SYSCALL_COUNT
                    .with_label_values(&[&vm_hash_str, &syscall_name])
                    .inc_by(count as f64);
            }
        }

        Ok(())
    }
}

/// Key structure matching the eBPF program's SyscallKey
#[repr(C)]
#[derive(Clone, Copy)]
struct SyscallKey {
    vm_id: u64,
    syscall_nr: u32,
    _pad: u32,
}

// Required for HashMap key
unsafe impl aya::Pod for SyscallKey {}

/// Hash a VM ID string to a u64 for use as a BPF map key
fn hash_vm_id(vm_id: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    vm_id.hash(&mut hasher);
    hasher.finish()
}

/// Convert syscall number to name (Linux x86_64)
fn syscall_name(nr: u32) -> String {
    // Common syscalls - expand as needed
    match nr {
        0 => "read".to_string(),
        1 => "write".to_string(),
        2 => "open".to_string(),
        3 => "close".to_string(),
        4 => "stat".to_string(),
        5 => "fstat".to_string(),
        6 => "lstat".to_string(),
        7 => "poll".to_string(),
        8 => "lseek".to_string(),
        9 => "mmap".to_string(),
        10 => "mprotect".to_string(),
        11 => "munmap".to_string(),
        12 => "brk".to_string(),
        13 => "rt_sigaction".to_string(),
        14 => "rt_sigprocmask".to_string(),
        20 => "writev".to_string(),
        21 => "access".to_string(),
        22 => "pipe".to_string(),
        23 => "select".to_string(),
        35 => "nanosleep".to_string(),
        39 => "getpid".to_string(),
        56 => "clone".to_string(),
        57 => "fork".to_string(),
        59 => "execve".to_string(),
        60 => "exit".to_string(),
        61 => "wait4".to_string(),
        62 => "kill".to_string(),
        102 => "getuid".to_string(),
        104 => "getgid".to_string(),
        110 => "getppid".to_string(),
        202 => "futex".to_string(),
        228 => "clock_gettime".to_string(),
        230 => "clock_nanosleep".to_string(),
        231 => "exit_group".to_string(),
        257 => "openat".to_string(),
        262 => "newfstatat".to_string(),
        _ => format!("syscall_{}", nr),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_vm_id() {
        let hash1 = hash_vm_id("vm-123");
        let hash2 = hash_vm_id("vm-123");
        let hash3 = hash_vm_id("vm-456");

        assert_eq!(hash1, hash2); // Same input, same hash
        assert_ne!(hash1, hash3); // Different input, different hash
    }

    #[test]
    fn test_syscall_name() {
        assert_eq!(syscall_name(0), "read");
        assert_eq!(syscall_name(1), "write");
        assert_eq!(syscall_name(59), "execve");
        assert_eq!(syscall_name(9999), "syscall_9999");
    }
}
