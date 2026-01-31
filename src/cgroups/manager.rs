//! cgroup v2 resource isolation for Firecracker VMs
//!
//! This module provides CPU and memory limits for VMs using Linux cgroups v2.
//! Each VM gets its own cgroup under /sys/fs/cgroup/neurovisor/{vm_id}/
//!
//! # How cgroups v2 Works
//!
//! cgroups (control groups) is a Linux kernel feature that limits, accounts for,
//! and isolates resource usage (CPU, memory, I/O) of process groups.
//!
//! ```text
//! /sys/fs/cgroup/                     ← cgroup v2 root
//! └── neurovisor/                     ← our namespace
//!     ├── vm-1/                       ← per-VM cgroup
//!     │   ├── cpu.max                 ← CPU limit: "200000 100000" = 2 cores
//!     │   ├── memory.max              ← Memory limit in bytes
//!     │   └── cgroup.procs            ← PIDs in this cgroup
//!     └── vm-2/
//!         └── ...
//! ```
//!
//! When a process is added to a cgroup, the kernel enforces the limits:
//! - CPU: Process gets throttled if it exceeds its quota
//! - Memory: Process gets OOM-killed if it exceeds its limit

use std::fs;
use std::io;
use std::path::PathBuf;

/// Base path for cgroup v2 filesystem
const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Our namespace within the cgroup hierarchy
const CGROUP_NAMESPACE: &str = "neurovisor";

/// Resource limits for a VM
///
/// # CPU Quota Explained
///
/// cgroups v2 uses `cpu.max` with format: "{quota} {period}"
/// - period: Time slice in microseconds (usually 100000 = 100ms)
/// - quota: How many microseconds the cgroup can use per period
///
/// Examples:
/// - "100000 100000" = 1 CPU core (100% of one core)
/// - "200000 100000" = 2 CPU cores
/// - "50000 100000"  = 0.5 CPU cores (50% of one core)
#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// Number of CPU cores (can be fractional, e.g., 0.5 for half a core)
    pub cpu_cores: f64,

    /// Memory limit in bytes
    pub memory_bytes: u64,
}

impl ResourceLimits {
    /// Create limits for a small VM (1 core, 2GB RAM)
    pub fn small() -> Self {
        Self {
            cpu_cores: 1.0,
            memory_bytes: 2 * 1024 * 1024 * 1024, // 2GB
        }
    }

    /// Create limits for a medium VM (2 cores, 4GB RAM)
    pub fn medium() -> Self {
        Self {
            cpu_cores: 2.0,
            memory_bytes: 4 * 1024 * 1024 * 1024, // 4GB
        }
    }

    /// Create limits for a large VM (4 cores, 8GB RAM)
    pub fn large() -> Self {
        Self {
            cpu_cores: 4.0,
            memory_bytes: 8 * 1024 * 1024 * 1024, // 8GB
        }
    }

    /// Create custom limits
    pub fn custom(cpu_cores: f64, memory_gb: f64) -> Self {
        Self {
            cpu_cores,
            memory_bytes: (memory_gb * 1024.0 * 1024.0 * 1024.0) as u64,
        }
    }
}

/// Manages cgroup lifecycle for VMs
///
/// # Example
///
/// ```ignore
/// let manager = CgroupManager::new()?;
///
/// // Create cgroup with limits before spawning Firecracker
/// manager.create("vm-1", ResourceLimits::medium())?;
///
/// // After spawning Firecracker, add its PID to the cgroup
/// manager.add_process("vm-1", firecracker_pid)?;
///
/// // When VM is destroyed, clean up the cgroup
/// manager.destroy("vm-1")?;
/// ```
pub struct CgroupManager {
    /// Base path: /sys/fs/cgroup/neurovisor
    base_path: PathBuf,
}

impl CgroupManager {
    /// Create a new CgroupManager
    ///
    /// This creates the neurovisor namespace directory if it doesn't exist,
    /// and enables the cpu and memory controllers for child cgroups.
    /// Requires root privileges or appropriate cgroup permissions.
    ///
    /// # cgroups v2 Controller Delegation
    ///
    /// In cgroups v2, controllers must be explicitly enabled at the parent
    /// level before children can use them. We write "+cpu +memory" to
    /// `cgroup.subtree_control` to enable these controllers for VM cgroups.
    pub fn new() -> io::Result<Self> {
        let base_path = PathBuf::from(CGROUP_ROOT).join(CGROUP_NAMESPACE);

        // Create the neurovisor namespace if it doesn't exist
        if !base_path.exists() {
            fs::create_dir_all(&base_path)?;
        }

        // Enable cpu and memory controllers for child cgroups
        // This is required in cgroups v2 - children can only use controllers
        // that are explicitly enabled in their parent's subtree_control
        let subtree_control = base_path.join("cgroup.subtree_control");
        fs::write(&subtree_control, "+cpu +memory")?;

        Ok(Self { base_path })
    }

    /// Get the path to a VM's cgroup directory
    fn vm_path(&self, vm_id: &str) -> PathBuf {
        self.base_path.join(vm_id)
    }

    /// Create a cgroup for a VM with resource limits
    ///
    /// # Arguments
    /// * `vm_id` - Unique identifier for the VM (e.g., "vm-1")
    /// * `limits` - Resource limits to apply
    ///
    /// # What This Does
    ///
    /// 1. Creates directory: /sys/fs/cgroup/neurovisor/{vm_id}/
    /// 2. Writes CPU limit to cpu.max
    /// 3. Writes memory limit to memory.max
    ///
    /// The cgroup is ready to accept processes after this call.
    pub fn create(&self, vm_id: &str, limits: ResourceLimits) -> io::Result<()> {
        let cgroup_path = self.vm_path(vm_id);

        // Create the cgroup directory
        // The kernel automatically creates control files (cpu.max, memory.max, etc.)
        fs::create_dir_all(&cgroup_path)?;

        // Set CPU limit
        // Format: "{quota} {period}" where both are in microseconds
        // period is typically 100000 (100ms)
        // quota = cores * period (e.g., 2 cores = 200000)
        let cpu_period: u64 = 100_000; // 100ms in microseconds
        let cpu_quota = (limits.cpu_cores * cpu_period as f64) as u64;
        let cpu_max = format!("{} {}", cpu_quota, cpu_period);

        fs::write(cgroup_path.join("cpu.max"), cpu_max)?;

        // Set memory limit
        // Just write the number of bytes directly
        fs::write(
            cgroup_path.join("memory.max"),
            limits.memory_bytes.to_string(),
        )?;

        Ok(())
    }

    /// Add a process to a VM's cgroup
    ///
    /// # Arguments
    /// * `vm_id` - The VM's cgroup to add the process to
    /// * `pid` - The process ID to add
    ///
    /// # What This Does
    ///
    /// Writes the PID to the cgroup's `cgroup.procs` file.
    /// The kernel then:
    /// - Moves the process into this cgroup
    /// - Starts enforcing resource limits immediately
    /// - Also moves all child processes/threads
    ///
    /// This should be called RIGHT AFTER spawning Firecracker.
    pub fn add_process(&self, vm_id: &str, pid: u32) -> io::Result<()> {
        let cgroup_path = self.vm_path(vm_id);
        let procs_file = cgroup_path.join("cgroup.procs");

        // Writing a PID to cgroup.procs moves that process into the cgroup
        fs::write(procs_file, pid.to_string())?;

        Ok(())
    }

    /// Destroy a VM's cgroup
    ///
    /// # What This Does
    ///
    /// Removes the cgroup directory. This will fail if there are still
    /// processes in the cgroup, so make sure the VM is stopped first.
    ///
    /// # Note
    ///
    /// The kernel doesn't allow removing a cgroup with active processes.
    /// You must either:
    /// - Kill all processes in the cgroup first, OR
    /// - Move them to another cgroup
    pub fn destroy(&self, vm_id: &str) -> io::Result<()> {
        let cgroup_path = self.vm_path(vm_id);

        if cgroup_path.exists() {
            // rmdir (not rm -rf) - the kernel requires this
            fs::remove_dir(&cgroup_path)?;
        }

        Ok(())
    }

    /// Check if a VM's cgroup exists
    pub fn exists(&self, vm_id: &str) -> bool {
        self.vm_path(vm_id).exists()
    }

    /// Get current memory usage for a VM (in bytes)
    ///
    /// Reads from memory.current which shows actual usage
    pub fn get_memory_usage(&self, vm_id: &str) -> io::Result<u64> {
        let cgroup_path = self.vm_path(vm_id);
        let content = fs::read_to_string(cgroup_path.join("memory.current"))?;
        content
            .trim()
            .parse()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Get current CPU usage statistics for a VM
    ///
    /// Returns (usage_usec, user_usec, system_usec) from cpu.stat
    pub fn get_cpu_stats(&self, vm_id: &str) -> io::Result<CpuStats> {
        let cgroup_path = self.vm_path(vm_id);
        let content = fs::read_to_string(cgroup_path.join("cpu.stat"))?;

        let mut stats = CpuStats::default();

        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() == 2 {
                let value: u64 = parts[1].parse().unwrap_or(0);
                match parts[0] {
                    "usage_usec" => stats.usage_usec = value,
                    "user_usec" => stats.user_usec = value,
                    "system_usec" => stats.system_usec = value,
                    "nr_throttled" => stats.nr_throttled = value,
                    "throttled_usec" => stats.throttled_usec = value,
                    _ => {}
                }
            }
        }

        Ok(stats)
    }

    /// List all VM cgroups under our namespace
    pub fn list_vms(&self) -> io::Result<Vec<String>> {
        let mut vms = Vec::new();

        if self.base_path.exists() {
            for entry in fs::read_dir(&self.base_path)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        vms.push(name.to_string());
                    }
                }
            }
        }

        Ok(vms)
    }
}

/// CPU statistics from cpu.stat
#[derive(Debug, Default, Clone)]
pub struct CpuStats {
    /// Total CPU time consumed (microseconds)
    pub usage_usec: u64,
    /// User-mode CPU time (microseconds)
    pub user_usec: u64,
    /// Kernel-mode CPU time (microseconds)
    pub system_usec: u64,
    /// Number of times the cgroup was throttled
    pub nr_throttled: u64,
    /// Total time spent throttled (microseconds)
    pub throttled_usec: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resource_limits_presets() {
        let small = ResourceLimits::small();
        assert_eq!(small.cpu_cores, 1.0);
        assert_eq!(small.memory_bytes, 2 * 1024 * 1024 * 1024);

        let medium = ResourceLimits::medium();
        assert_eq!(medium.cpu_cores, 2.0);
        assert_eq!(medium.memory_bytes, 4 * 1024 * 1024 * 1024);

        let large = ResourceLimits::large();
        assert_eq!(large.cpu_cores, 4.0);
        assert_eq!(large.memory_bytes, 8 * 1024 * 1024 * 1024);
    }

    #[test]
    fn test_custom_limits() {
        let custom = ResourceLimits::custom(1.5, 3.0);
        assert_eq!(custom.cpu_cores, 1.5);
        assert_eq!(custom.memory_bytes, (3.0 * 1024.0 * 1024.0 * 1024.0) as u64);
    }
}
