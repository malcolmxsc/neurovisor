//! eBPF LSM loader for file access control
//!
//! This module loads and manages the LSM BPF program for enforcing
//! file access policies on tracked Firecracker processes.

#[cfg(feature = "ebpf")]
use aya::maps::HashMap;
#[cfg(feature = "ebpf")]
use aya::programs::Lsm;
#[cfg(feature = "ebpf")]
use aya::{include_bytes_aligned, Bpf, Btf};
#[cfg(feature = "ebpf")]
use std::sync::Arc;
#[cfg(feature = "ebpf")]
use tokio::sync::RwLock;

use super::policy::SecurityPolicy;

/// Manages the LSM BPF program for file access control.
pub struct LsmManager {
    #[cfg(feature = "ebpf")]
    bpf: Arc<RwLock<Bpf>>,
    policy: SecurityPolicy,
    enabled: bool,
}

/// Error type for LSM operations
#[derive(Debug)]
pub enum LsmError {
    /// Failed to load LSM program
    LoadError(String),
    /// Failed to attach LSM program
    AttachError(String),
    /// Failed to access BPF map
    MapError(String),
    /// LSM BPF not supported by kernel
    NotSupported(String),
}

impl std::fmt::Display for LsmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LsmError::LoadError(msg) => write!(f, "LSM load error: {}", msg),
            LsmError::AttachError(msg) => write!(f, "LSM attach error: {}", msg),
            LsmError::MapError(msg) => write!(f, "LSM map error: {}", msg),
            LsmError::NotSupported(msg) => write!(f, "LSM not supported: {}", msg),
        }
    }
}

impl std::error::Error for LsmError {}

impl LsmManager {
    /// Create a new LSM manager with the given security policy.
    ///
    /// Returns None if LSM BPF is not supported or fails to load.
    pub fn new(policy: SecurityPolicy) -> Option<Self> {
        match Self::try_new(policy.clone()) {
            Ok(manager) => {
                println!("[LSM] File access control enabled");
                println!(
                    "[LSM] Blocking {} path prefixes",
                    policy.blocked_paths.len()
                );
                Some(manager)
            }
            Err(e) => {
                println!("[LSM] Failed to initialize: {} (continuing without LSM)", e);
                None
            }
        }
    }

    #[cfg(feature = "ebpf")]
    fn try_new(policy: SecurityPolicy) -> Result<Self, LsmError> {
        // Check if kernel supports BPF LSM
        if !Self::check_lsm_support() {
            return Err(LsmError::NotSupported(
                "BPF not in LSM list (check /sys/kernel/security/lsm)".to_string(),
            ));
        }

        // Load BTF for CO-RE
        let btf = Btf::from_sys_fs().map_err(|e| LsmError::LoadError(e.to_string()))?;

        // Load the pre-compiled LSM BPF bytecode
        let bpf_bytes = include_bytes_aligned!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/target/ebpf/lsm-file-open.o"
        ));

        let mut bpf =
            Bpf::load(bpf_bytes).map_err(|e| LsmError::LoadError(e.to_string()))?;

        // Attach to the file_open LSM hook
        let program: &mut Lsm = bpf
            .program_mut("file_open_check")
            .ok_or_else(|| LsmError::LoadError("file_open_check program not found".to_string()))?
            .try_into()
            .map_err(|e: aya::programs::ProgramError| LsmError::LoadError(e.to_string()))?;

        program
            .load("file_open", &btf)
            .map_err(|e| LsmError::LoadError(e.to_string()))?;

        program
            .attach()
            .map_err(|e| LsmError::AttachError(e.to_string()))?;

        let manager = Self {
            bpf: Arc::new(RwLock::new(bpf)),
            policy,
            enabled: true,
        };

        // Populate blocked paths map
        // This would be done here but requires async context
        // manager.sync_blocked_paths().await?;

        Ok(manager)
    }

    #[cfg(not(feature = "ebpf"))]
    fn try_new(_policy: SecurityPolicy) -> Result<Self, LsmError> {
        Err(LsmError::NotSupported(
            "eBPF feature not enabled".to_string(),
        ))
    }

    /// Check if the kernel has BPF in the LSM list
    #[cfg(feature = "ebpf")]
    fn check_lsm_support() -> bool {
        if let Ok(lsm) = std::fs::read_to_string("/sys/kernel/security/lsm") {
            lsm.contains("bpf")
        } else {
            false
        }
    }

    /// Check if LSM enforcement is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get the current security policy
    pub fn policy(&self) -> &SecurityPolicy {
        &self.policy
    }

    /// Register a PID for LSM tracking
    #[cfg(feature = "ebpf")]
    pub async fn track_pid(&self, pid: u32) -> Result<(), LsmError> {
        let mut bpf = self.bpf.write().await;
        let mut pid_map: HashMap<_, u32, u8> = bpf
            .map_mut("TRACKED_PIDS")
            .ok_or_else(|| LsmError::MapError("TRACKED_PIDS map not found".to_string()))?
            .try_into()
            .map_err(|e: aya::maps::MapError| LsmError::MapError(e.to_string()))?;

        pid_map
            .insert(pid, 1, 0)
            .map_err(|e| LsmError::MapError(e.to_string()))?;

        println!("[LSM] Tracking PID {}", pid);
        Ok(())
    }

    #[cfg(not(feature = "ebpf"))]
    pub async fn track_pid(&self, _pid: u32) -> Result<(), LsmError> {
        Ok(())
    }

    /// Unregister a PID from LSM tracking
    #[cfg(feature = "ebpf")]
    pub async fn untrack_pid(&self, pid: u32) -> Result<(), LsmError> {
        let mut bpf = self.bpf.write().await;
        let mut pid_map: HashMap<_, u32, u8> = bpf
            .map_mut("TRACKED_PIDS")
            .ok_or_else(|| LsmError::MapError("TRACKED_PIDS map not found".to_string()))?
            .try_into()
            .map_err(|e: aya::maps::MapError| LsmError::MapError(e.to_string()))?;

        let _ = pid_map.remove(&pid);
        println!("[LSM] Untracking PID {}", pid);
        Ok(())
    }

    #[cfg(not(feature = "ebpf"))]
    pub async fn untrack_pid(&self, _pid: u32) -> Result<(), LsmError> {
        Ok(())
    }

    /// Sync blocked paths from policy to eBPF map
    #[cfg(feature = "ebpf")]
    pub async fn sync_blocked_paths(&self) -> Result<(), LsmError> {
        let mut bpf = self.bpf.write().await;
        let mut path_map: HashMap<_, [u8; 64], u8> = bpf
            .map_mut("BLOCKED_PATHS")
            .ok_or_else(|| LsmError::MapError("BLOCKED_PATHS map not found".to_string()))?
            .try_into()
            .map_err(|e: aya::maps::MapError| LsmError::MapError(e.to_string()))?;

        // Clear existing entries (not directly supported, so we just add new ones)
        // In production, we'd track what's in the map

        for path_bytes in self.policy.paths_as_bytes() {
            path_map
                .insert(path_bytes, 1, 0)
                .map_err(|e| LsmError::MapError(e.to_string()))?;
        }

        println!(
            "[LSM] Synced {} blocked paths to eBPF map",
            self.policy.blocked_paths.len()
        );
        Ok(())
    }

    #[cfg(not(feature = "ebpf"))]
    pub async fn sync_blocked_paths(&self) -> Result<(), LsmError> {
        Ok(())
    }

    /// Get the total count of blocked access attempts
    #[cfg(feature = "ebpf")]
    pub async fn blocked_count(&self) -> u64 {
        let bpf = self.bpf.read().await;
        if let Some(map) = bpf.map("BLOCKED_TOTAL") {
            if let Ok(counts) = HashMap::<_, u32, u64>::try_from(map) {
                return counts.get(&0, 0).unwrap_or(0);
            }
        }
        0
    }

    #[cfg(not(feature = "ebpf"))]
    pub async fn blocked_count(&self) -> u64 {
        0
    }

    /// Collect per-path blocked counts and export to Prometheus metrics
    #[cfg(feature = "ebpf")]
    pub async fn collect_metrics(&self) -> Result<(), LsmError> {
        use crate::ebpf::metrics::{EBPF_LSM_BLOCKED, EBPF_LSM_BLOCKED_TOTAL};

        let bpf = self.bpf.read().await;

        // Read per-path blocked counts
        if let Some(map) = bpf.map("BLOCKED_PATH_COUNTS") {
            if let Ok(path_counts) = HashMap::<_, [u8; 64], u64>::try_from(map) {
                for item in path_counts.iter() {
                    if let Ok((path_bytes, count)) = item {
                        // Convert path bytes to string
                        let path = bytes_to_path(&path_bytes);
                        if !path.is_empty() && count > 0 {
                            EBPF_LSM_BLOCKED
                                .with_label_values(&[&path])
                                .inc_by(count as f64);
                        }
                    }
                }
            }
        }

        // Read total blocked count
        if let Some(map) = bpf.map("BLOCKED_TOTAL") {
            if let Ok(total_map) = HashMap::<_, u32, u64>::try_from(map) {
                if let Ok(total) = total_map.get(&0, 0) {
                    EBPF_LSM_BLOCKED_TOTAL.set(total as f64);
                }
            }
        }

        Ok(())
    }

    #[cfg(not(feature = "ebpf"))]
    pub async fn collect_metrics(&self) -> Result<(), LsmError> {
        Ok(())
    }
}

/// Convert path bytes to a string, trimming null bytes
#[cfg(feature = "ebpf")]
fn bytes_to_path(bytes: &[u8; 64]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(64);
    String::from_utf8_lossy(&bytes[..end]).to_string()
}
