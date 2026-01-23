use std::process::Stdio;
use std::io::Write; // Adds the .write_all() capability
use std::{thread, time};
use tokio::time::Duration as TokioDuration;

// Import our new modules
use neurovisor::vm::{spawn_firecracker, wait_for_api_socket, FirecrackerClient, to_absolute_path};

const API_SOCKET: &str = "/tmp/firecracker.socket";
const KERNEL_PATH: &str = "./vmlinux";
const ROOTFS_PATH: &str = "./rootfs.ext4";
const VSOCK_PATH: &str = "./neurovisor.vsock";
const SNAPSHOT_PATH: &str = "./snapshot_file";
const MEM_PATH: &str = "./mem_file";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🏭 STARTING SNAPSHOT FACTORY...");

    // 1. CLEANUP (The Builder IS allowed to delete files)
    let _ = std::fs::remove_file(API_SOCKET);
    let _ = std::fs::remove_file(VSOCK_PATH);
    let _ = std::fs::remove_file(SNAPSHOT_PATH);
    let _ = std::fs::remove_file(MEM_PATH);

    // 2. Launch Firecracker (with piped stdin for automated commands)
    let mut child = spawn_firecracker(API_SOCKET, Stdio::piped())?;

    // 3. Wait for API (With Safety Deadline of 5 seconds)
    wait_for_api_socket(API_SOCKET, Some(TokioDuration::from_secs(5)))?;

    // 4. Create Firecracker client
    let fc = FirecrackerClient::new(API_SOCKET);

    // 5. Configure Resources
    println!("[INFO] Configuring VM resources...");
    fc.boot_source(
        KERNEL_PATH,
        "console=ttyS0 reboot=k panic=1 pci=off ip=172.16.0.2::172.16.0.1:255.255.255.0::eth0:off root=/dev/vda rw",
    )
    .await?;

    fc.add_drive("rootfs", ROOTFS_PATH, true, false).await?;
    fc.configure_vsock(3, VSOCK_PATH).await?;

    // 6. BOOT
    println!("🔥 BOOTING... (Login as root/root, run 'python3 agent.py &')\r");
    fc.start().await?;

    // 7. COUNTDOWN (60 seconds)
    // A. Wait for Boot (20 seconds for the VM to reach login screen)
    println!("⏳ Waiting 20s for login prompt...");
    thread::sleep(time::Duration::from_secs(30));

    // B. The Robot Typer
    // .take() grabs the stdin handle so we can use it
    if let Some(mut vm_stdin) = child.stdin.take() {
        println!("🤖 ROBOT: Typing credentials...");
        
        vm_stdin.write_all(b"root\n")?;        // Username
        thread::sleep(time::Duration::from_secs(3)); // Small pause

        vm_stdin.write_all(b"root\n")?;        // Password
        thread::sleep(time::Duration::from_secs(3));

        println!("[INFO] 🤖 ROBOT: Starting GPU Bridge Agent...");
        // Standardize: Kill previous instances to ensure a clean snapshot state
        vm_stdin.write_all(b"pkill -9 python3\n")?; 
        thread::sleep(time::Duration::from_secs(2));
        
        // Execute the GPU client logic that connects to Port 6000
        vm_stdin.write_all(b"python3 smart_agent.py &\n")?;
    }

    // C. Wait for Agent (5 seconds for Python to start listening)
    println!("⏳ Waiting 5s for agent to warm up...");
    thread::sleep(time::Duration::from_secs(5));

    // 8. PAUSE & SNAPSHOT
    println!("[INFO] ⏸️  SUSPENDING VM STATE...");
    fc.pause().await?;

    println!("[INFO] 📸 CAPTURING FULL SNAPSHOT...");
    let snap_abs = to_absolute_path(SNAPSHOT_PATH)?;
    let mem_abs = to_absolute_path(MEM_PATH)?;

    fc.create_snapshot(&snap_abs, &mem_abs).await?;

    println!("[INFO] ✅ SNAPSHOT COMPLETE!");

    // 9. Cleanup & Exit
    child.kill()?;
    Ok(())
}