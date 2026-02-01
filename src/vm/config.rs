//! Firecracker VM configuration structures
//!
//! These structs represent the JSON payloads used in Firecracker API requests.

use serde::Serialize;

/// Boot source configuration for the VM kernel
#[derive(Serialize, Debug, Clone)]
pub struct BootSource {
    pub kernel_image_path: String,
    pub boot_args: String,
}

/// Block device (drive) configuration
#[derive(Serialize, Debug, Clone)]
pub struct Drive {
    pub drive_id: String,
    pub path_on_host: String,
    pub is_root_device: bool,
    pub is_read_only: bool,
}

/// Virtio-vsock device configuration for host-guest communication
#[derive(Serialize, Debug, Clone)]
pub struct Vsock {
    pub guest_cid: u32,
    pub uds_path: String,
}

/// VM action (e.g., "InstanceStart")
#[derive(Serialize, Debug, Clone)]
pub struct Action {
    pub action_type: String,
}

/// VM state change (e.g., "Paused")
#[derive(Serialize, Debug, Clone)]
pub struct VmState {
    pub state: String,
}

/// Snapshot creation configuration
#[derive(Serialize, Debug, Clone)]
pub struct SnapshotConfig {
    pub snapshot_type: String,
    pub snapshot_path: String,
    pub mem_file_path: String,
}

/// Memory backend configuration for snapshot loading
#[derive(Serialize, Debug, Clone)]
pub struct MemBackend {
    pub backend_type: String,
    pub backend_path: String,
}

/// Snapshot load configuration (v1.14+ API)
#[derive(Serialize, Debug, Clone)]
pub struct SnapshotLoad {
    pub snapshot_path: String,
    pub mem_backend: MemBackend,
    pub resume_vm: bool,
}
