//! VM Manager - creates and destroys individual VMs
//!
//! The VMManager is responsible for:
//! - Allocating unique CIDs for vsock communication
//! - Spawning Firecracker processes with unique socket paths
//! - Configuring VMs (boot source, drives, vsock)
//! - Setting up cgroup resource limits
//! - Cleaning up VM resources on destruction

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use tokio::time::Duration;
use uuid::Uuid;

use super::handle::VMHandle;
use super::{spawn_firecracker, wait_for_api_socket, to_absolute_path, FirecrackerClient};
use crate::cgroups::{CgroupManager, ResourceLimits};
use crate::ebpf::{EbpfManager, TraceManager};
use crate::metrics::VM_BOOT_DURATION;

/// Configuration for the VMManager
#[derive(Debug, Clone)]
pub struct VMManagerConfig {
    /// Path to the kernel image
    pub kernel_path: PathBuf,
    /// Path to the root filesystem
    pub rootfs_path: PathBuf,
    /// Path to snapshot file (if using snapshot boot)
    pub snapshot_path: Option<PathBuf>,
    /// Path to memory file (if using snapshot boot)
    pub mem_path: Option<PathBuf>,
    /// Resource limits for each VM
    pub resource_limits: ResourceLimits,
    /// Vsock port for gRPC communication
    pub vsock_port: u32,
}

impl Default for VMManagerConfig {
    fn default() -> Self {
        Self {
            kernel_path: PathBuf::from("./vmlinuz"),
            rootfs_path: PathBuf::from("./rootfs.ext4"),
            snapshot_path: None,
            mem_path: None,
            resource_limits: ResourceLimits::medium(),
            vsock_port: 6000,
        }
    }
}

/// Manages creation and destruction of individual VMs
pub struct VMManager {
    /// Next CID to allocate (starts at 3, increments atomically)
    /// CID 0 is reserved, 1 is host, 2 is reserved, so we start at 3
    next_cid: AtomicU32,
    /// Cgroup manager for resource limits (None if cgroups unavailable)
    cgroup_manager: Option<CgroupManager>,
    /// eBPF manager for syscall tracing (None if eBPF unavailable)
    ebpf_manager: Option<EbpfManager>,
    /// Trace manager for distributed tracing (None if unavailable)
    trace_manager: Option<TraceManager>,
    /// Configuration for VMs
    config: VMManagerConfig,
}

impl VMManager {
    /// Create a new VMManager
    ///
    /// Attempts to initialize cgroup manager, but continues with graceful
    /// degradation if cgroups are unavailable.
    pub fn new(config: VMManagerConfig) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // Try to create cgroup manager, but don't fail if unavailable
        let cgroup_manager = match CgroupManager::new() {
            Ok(m) => {
                println!("[INFO] ✅ CGROUP MANAGER INITIALIZED");
                Some(m)
            }
            Err(e) => {
                eprintln!("[WARN] Failed to initialize cgroup manager: {}", e);
                eprintln!("[WARN] VMs will run without resource limits");
                None
            }
        };

        // Try to create eBPF manager for syscall tracing (graceful degradation)
        let ebpf_manager = EbpfManager::new();
        if ebpf_manager.is_some() {
            crate::ebpf::metrics::set_enabled(true);
        }

        // Try to create trace manager for distributed tracing (graceful degradation)
        let trace_manager = TraceManager::new();

        Ok(Self {
            next_cid: AtomicU32::new(3), // Start at 3 (0,1,2 are reserved)
            cgroup_manager,
            ebpf_manager,
            trace_manager,
            config,
        })
    }

    /// Allocate a unique CID for a new VM
    fn allocate_cid(&self) -> u32 {
        self.next_cid.fetch_add(1, Ordering::SeqCst)
    }

    /// Create a new VM and return a handle to it
    ///
    /// This spawns a Firecracker process, configures it, and boots the VM.
    /// Returns a VMHandle with status = Ready when complete.
    pub async fn create_vm(&self) -> Result<VMHandle, Box<dyn std::error::Error + Send + Sync>> {
        let start_time = Instant::now();

        // Generate unique VM ID
        let vm_id = format!("vm-{}", Uuid::now_v7());
        let cid = self.allocate_cid();

        // Generate unique paths for this VM
        let api_socket = PathBuf::from(format!("/tmp/firecracker-{}.socket", vm_id));
        let vsock_path = PathBuf::from(format!("./neurovisor-{}.vsock", vm_id));

        // Clean up any stale files from previous runs
        let _ = std::fs::remove_file(&api_socket);
        let _ = std::fs::remove_file(&vsock_path);

        println!("[INFO] CREATING VM {} (CID: {})", vm_id, cid);

        // Spawn Firecracker process
        let api_socket_str = api_socket.to_str().unwrap();
        let process = spawn_firecracker(api_socket_str, Stdio::null())
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            })?;

        let firecracker_pid = process.id();

        // Set up cgroup for this VM
        if let Some(ref cgroup_mgr) = self.cgroup_manager {
            if let Err(e) = cgroup_mgr.create(&vm_id, self.config.resource_limits.clone()) {
                eprintln!("[WARN] Failed to create cgroup for {}: {}", vm_id, e);
            } else if let Err(e) = cgroup_mgr.add_process(&vm_id, firecracker_pid) {
                eprintln!("[WARN] Failed to add PID {} to cgroup: {}", firecracker_pid, e);
            } else {
                println!("[INFO]    Cgroup created for {} (PID: {})", vm_id, firecracker_pid);
            }
        }

        // Start eBPF syscall tracing for this VM
        if let Some(ref ebpf_mgr) = self.ebpf_manager {
            if let Err(e) = ebpf_mgr.start_tracing(&vm_id, firecracker_pid).await {
                eprintln!("[WARN] Failed to start eBPF tracing for {}: {}", vm_id, e);
            }
        }

        // Wait for Firecracker API socket
        wait_for_api_socket(api_socket_str, Some(Duration::from_secs(10)))
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(std::io::Error::new(std::io::ErrorKind::TimedOut, e.to_string()))
            })?;

        // Create Firecracker API client
        let fc_client = FirecrackerClient::new(api_socket_str);

        // Create VMHandle
        let mut handle = VMHandle::new(
            vm_id.clone(),
            cid,
            process,
            api_socket,
            vsock_path.clone(),
            fc_client,
        );

        // Configure and boot the VM
        if let (Some(snap_path), Some(mem_path)) = (&self.config.snapshot_path, &self.config.mem_path) {
            // Snapshot boot path (faster)
            self.configure_vm_from_snapshot(&handle, snap_path, mem_path).await?;
        } else {
            // Fresh boot path
            self.configure_vm_fresh(&handle, &vsock_path, cid).await?;
        }

        // Mark VM as ready
        handle.mark_ready();

        let boot_duration = start_time.elapsed();
        VM_BOOT_DURATION.observe(boot_duration.as_secs_f64());
        println!("[INFO] ✅ VM {} READY (boot time: {:.2}s)", vm_id, boot_duration.as_secs_f64());

        Ok(handle)
    }

    /// Configure VM for fresh boot (kernel + rootfs)
    async fn configure_vm_fresh(
        &self,
        handle: &VMHandle,
        vsock_path: &PathBuf,
        cid: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let kernel_abs = to_absolute_path(self.config.kernel_path.to_str().unwrap())
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            })?;
        let rootfs_abs = to_absolute_path(self.config.rootfs_path.to_str().unwrap())
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            })?;

        // Configure boot source
        handle.fc_client.boot_source(
            &kernel_abs,
            "console=ttyS0 reboot=k panic=1 pci=off quiet loglevel=0 root=/dev/vda rw init=/usr/local/bin/run_guest.sh",
        ).await.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
            Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
        })?;

        // Add root drive
        handle.fc_client.add_drive("root", &rootfs_abs, true, false)
            .await.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            })?;

        // Configure vsock with unique CID
        let vsock_path_str = vsock_path.to_str().unwrap();
        handle.fc_client.configure_vsock(cid, vsock_path_str)
            .await.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            })?;

        // Start VM
        handle.fc_client.start()
            .await.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            })?;

        Ok(())
    }

    /// Configure VM from snapshot (faster boot)
    async fn configure_vm_from_snapshot(
        &self,
        handle: &VMHandle,
        snapshot_path: &PathBuf,
        mem_path: &PathBuf,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let snap_abs = to_absolute_path(snapshot_path.to_str().unwrap())
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            })?;
        let mem_abs = to_absolute_path(mem_path.to_str().unwrap())
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            })?;

        // Load snapshot
        handle.fc_client.load_snapshot(&snap_abs, &mem_abs, false)
            .await.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            })?;

        // Resume VM
        handle.fc_client.resume()
            .await.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
            })?;

        Ok(())
    }

    /// Destroy a VM and clean up all resources
    pub async fn destroy_vm(&self, mut handle: VMHandle) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let vm_id = handle.vm_id.clone();
        let firecracker_pid = handle.pid();
        println!("[INFO] DESTROYING VM {}", vm_id);

        // Stop eBPF tracing before shutdown
        if let Some(ref ebpf_mgr) = self.ebpf_manager {
            if let Err(e) = ebpf_mgr.stop_tracing(firecracker_pid).await {
                eprintln!("[WARN] Failed to stop eBPF tracing for {}: {}", vm_id, e);
            }
        }

        // Shutdown the VM
        handle.shutdown().await?;

        // Clean up cgroup
        if let Some(ref cgroup_mgr) = self.cgroup_manager {
            if let Err(e) = cgroup_mgr.destroy(&vm_id) {
                eprintln!("[WARN] Failed to destroy cgroup for {}: {}", vm_id, e);
            }
        }

        println!("[INFO] ✅ VM {} DESTROYED", vm_id);
        Ok(())
    }

    /// Get the vsock port configured for VMs
    pub fn vsock_port(&self) -> u32 {
        self.config.vsock_port
    }

    /// Check if using snapshot boot
    pub fn uses_snapshot(&self) -> bool {
        self.config.snapshot_path.is_some() && self.config.mem_path.is_some()
    }

    /// Start distributed tracing for a VM
    ///
    /// Associates a trace_id with the Firecracker process PID so that
    /// eBPF events from this VM can be correlated with the request trace.
    pub async fn start_trace(&self, handle: &VMHandle, trace_id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(ref trace_mgr) = self.trace_manager {
            let pid = handle.pid();
            trace_mgr.start_trace(pid, trace_id).await.map_err(|e| {
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
                    as Box<dyn std::error::Error + Send + Sync>
            })?;
            println!("[INFO]    Trace started for {} (trace_id: {})", handle.vm_id, trace_id);
        }
        Ok(())
    }

    /// Stop distributed tracing for a VM
    ///
    /// Removes the trace_id association before VM destruction.
    pub async fn stop_trace(&self, handle: &VMHandle) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(ref trace_mgr) = self.trace_manager {
            let pid = handle.pid();
            trace_mgr.stop_trace(pid).await.map_err(|e| {
                Box::new(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
                    as Box<dyn std::error::Error + Send + Sync>
            })?;
        }
        Ok(())
    }
}
