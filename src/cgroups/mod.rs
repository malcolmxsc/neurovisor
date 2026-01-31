//! cgroups v2 resource isolation module
//!
//! This module provides resource limits (CPU, memory) for Firecracker VMs
//! using Linux cgroups v2.
//!
//! # Why cgroups?
//!
//! Without cgroups, a runaway VM could consume all host CPU/memory,
//! starving other VMs and potentially crashing the host. cgroups provide
//! kernel-enforced resource limits that VMs cannot escape.
//!
//! # Example Usage
//!
//! ```ignore
//! use neurovisor::cgroups::{CgroupManager, ResourceLimits};
//!
//! // Create manager (creates /sys/fs/cgroup/neurovisor/ if needed)
//! let cgroups = CgroupManager::new()?;
//!
//! // Create cgroup with medium limits (2 cores, 4GB)
//! cgroups.create("vm-1", ResourceLimits::medium())?;
//!
//! // After spawning Firecracker, bind its PID
//! cgroups.add_process("vm-1", firecracker_pid)?;
//!
//! // Monitor resource usage
//! let memory = cgroups.get_memory_usage("vm-1")?;
//! let cpu = cgroups.get_cpu_stats("vm-1")?;
//!
//! // Clean up when VM is destroyed
//! cgroups.destroy("vm-1")?;
//! ```

pub mod manager;

pub use manager::{CgroupManager, CpuStats, ResourceLimits};
