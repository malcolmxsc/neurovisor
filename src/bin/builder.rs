//! Snapshot builder for Neurovisor VMs
//!
//! Boots a Firecracker VM with a shell init, waits for it to stabilize,
//! then pauses and captures a full snapshot for fast restore.

use std::process::Stdio;
use tokio::time::Duration;

use neurovisor::vm::{spawn_firecracker, wait_for_api_socket, FirecrackerClient, to_absolute_path};

const API_SOCKET: &str = "/tmp/firecracker.socket";
const KERNEL_PATH: &str = "./vmlinuz";
const ROOTFS_PATH: &str = "./rootfs.ext4";
const VSOCK_PATH: &str = "./neurovisor.vsock";
const SNAPSHOT_PATH: &str = "./snapshot_file";
const MEM_PATH: &str = "./mem_file";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("[INFO] STARTING SNAPSHOT BUILDER...");

    // 1. Cleanup stale files
    for path in [API_SOCKET, VSOCK_PATH, SNAPSHOT_PATH, MEM_PATH] {
        let _ = std::fs::remove_file(path);
    }

    // 2. Launch Firecracker
    let mut child = spawn_firecracker(API_SOCKET, Stdio::inherit())?;

    // 3. Wait for API
    wait_for_api_socket(API_SOCKET, Some(Duration::from_secs(5)))?;

    // 4. Configure VM
    let fc = FirecrackerClient::new(API_SOCKET);

    let kernel_abs = to_absolute_path(KERNEL_PATH)?;
    let rootfs_abs = to_absolute_path(ROOTFS_PATH)?;

    println!("[INFO] Configuring VM resources...");
    fc.boot_source(
        &kernel_abs,
        "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/bin/sh",
    )
    .await?;

    fc.add_drive("root", &rootfs_abs, true, false).await?;
    fc.configure_vsock(3, VSOCK_PATH).await?;

    // 5. Boot and let it stabilize
    println!("[INFO] Booting VM...");
    fc.start().await?;

    println!("[INFO] Waiting 5s for VM to stabilize...");
    tokio::time::sleep(Duration::from_secs(5)).await;

    // 6. Pause and snapshot
    println!("[INFO] Pausing VM...");
    fc.pause().await?;

    println!("[INFO] Capturing snapshot...");
    let snap_abs = to_absolute_path(SNAPSHOT_PATH)?;
    let mem_abs = to_absolute_path(MEM_PATH)?;
    fc.create_snapshot(&snap_abs, &mem_abs).await?;

    println!("[INFO] Snapshot complete: {} + {}", SNAPSHOT_PATH, MEM_PATH);

    // 7. Cleanup
    child.kill()?;
    println!("[INFO] Builder done.");
    Ok(())
}