//! VM Handle - represents a single VM instance with all its resources
//!
//! Each VMHandle tracks one Firecracker VM throughout its lifecycle:
//! Starting -> Ready -> Active -> Stopping

use std::path::PathBuf;
use std::process::Child;
use std::time::Instant;

use super::FirecrackerClient;

/// Status of a VM in the pool
#[derive(Debug, Clone, PartialEq)]
pub enum VMStatus {
    /// VM is starting up (booting or loading snapshot)
    Starting,
    /// VM is booted and ready for assignment (in warm pool)
    Ready,
    /// VM is assigned to a request and actively processing
    Active,
    /// VM is shutting down
    Stopping,
    /// VM failed with an error
    Failed(String),
}

/// Represents a single VM instance with all its resources
pub struct VMHandle {
    /// Unique identifier for this VM (e.g., "vm-01926abc...")
    pub vm_id: String,
    /// Vsock guest CID (3, 4, 5, ...) - must be unique per running VM
    pub cid: u32,
    /// Firecracker process handle
    pub process: Child,
    /// Path to Firecracker API socket (e.g., /tmp/firecracker-{vm_id}.socket)
    pub api_socket: PathBuf,
    /// Path to vsock UDS (e.g., ./neurovisor-{vm_id}.vsock)
    pub vsock_path: PathBuf,
    /// Firecracker API client for this VM
    pub fc_client: FirecrackerClient,
    /// Current status of this VM
    pub status: VMStatus,
    /// When this VM was created
    pub created_at: Instant,
}

impl VMHandle {
    /// Create a new VMHandle
    pub fn new(
        vm_id: String,
        cid: u32,
        process: Child,
        api_socket: PathBuf,
        vsock_path: PathBuf,
        fc_client: FirecrackerClient,
    ) -> Self {
        Self {
            vm_id,
            cid,
            process,
            api_socket,
            vsock_path,
            fc_client,
            status: VMStatus::Starting,
            created_at: Instant::now(),
        }
    }

    /// Mark VM as ready (booted and available for assignment)
    pub fn mark_ready(&mut self) {
        self.status = VMStatus::Ready;
    }

    /// Mark VM as active (assigned to a request)
    pub fn mark_active(&mut self) {
        self.status = VMStatus::Active;
    }

    /// Mark VM as failed
    pub fn mark_failed(&mut self, error: String) {
        self.status = VMStatus::Failed(error);
    }

    /// Check if VM is ready for assignment
    pub fn is_ready(&self) -> bool {
        matches!(self.status, VMStatus::Ready)
    }

    /// Get the vsock listener path for gRPC connections
    /// Format: {vsock_path}_{port}
    pub fn vsock_listener_path(&self, port: u32) -> PathBuf {
        let path_str = format!("{}_{}", self.vsock_path.display(), port);
        PathBuf::from(path_str)
    }

    /// Get time since VM was created
    pub fn age(&self) -> std::time::Duration {
        self.created_at.elapsed()
    }

    /// Graceful shutdown: send shutdown command via Firecracker API, then cleanup
    pub async fn shutdown(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.status = VMStatus::Stopping;

        // Try graceful shutdown via Firecracker API (send InstanceHalt if supported)
        // Firecracker doesn't have a halt API, so we just kill the process

        // Kill the Firecracker process
        if let Err(e) = self.process.kill() {
            // Process might already be dead
            eprintln!("[WARN] Failed to kill Firecracker process for {}: {}", self.vm_id, e);
        }

        // Wait for process to exit
        let _ = self.process.wait();

        // Cleanup socket files
        self.cleanup_files();

        Ok(())
    }

    /// Cleanup socket and vsock files
    pub fn cleanup_files(&self) {
        // Remove API socket
        if self.api_socket.exists() {
            if let Err(e) = std::fs::remove_file(&self.api_socket) {
                eprintln!("[WARN] Failed to remove API socket {}: {}", self.api_socket.display(), e);
            }
        }

        // Remove vsock path
        if self.vsock_path.exists() {
            if let Err(e) = std::fs::remove_file(&self.vsock_path) {
                eprintln!("[WARN] Failed to remove vsock path {}: {}", self.vsock_path.display(), e);
            }
        }

        // Remove vsock listener path (with port suffix)
        let listener_path = self.vsock_listener_path(6000);
        if listener_path.exists() {
            if let Err(e) = std::fs::remove_file(&listener_path) {
                eprintln!("[WARN] Failed to remove vsock listener {}: {}", listener_path.display(), e);
            }
        }
    }
}

impl std::fmt::Debug for VMHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VMHandle")
            .field("vm_id", &self.vm_id)
            .field("cid", &self.cid)
            .field("api_socket", &self.api_socket)
            .field("vsock_path", &self.vsock_path)
            .field("status", &self.status)
            .field("age", &self.age())
            .finish()
    }
}
