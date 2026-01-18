use std::process::{Command, Stdio};
use std::{thread, time};
use std::path::Path;
use serde::Serialize;
use std::io::Write; // Adds the .write_all() capability

#[derive(Serialize)]
struct BootSource { kernel_image_path: String, boot_args: String }

#[derive(Serialize)]
struct Drive { drive_id: String, path_on_host: String, is_root_device: bool, is_read_only: bool }

#[derive(Serialize)]
struct Vsock { guest_cid: u32, uds_path: String }

#[derive(Serialize)]
struct Action { action_type: String }

#[derive(Serialize)]
struct SnapshotConfig { snapshot_type: String, snapshot_path: String, mem_file_path: String }

#[derive(Serialize)]
struct VmState { state: String }

const FIRECRACKER_BIN: &str = "./firecracker";
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
    
    // 2. Launch Firecracker
    let mut child = Command::new(FIRECRACKER_BIN).arg("--api-sock").arg(API_SOCKET)
        // CHANGE THIS LINE ONLY:
        .stdin(Stdio::piped()) 
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    // 3. Wait for API
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build(hyperlocal::UnixConnector);
    while !Path::new(API_SOCKET).exists() { thread::sleep(time::Duration::from_millis(100)); }

    // 4. Configure Resources
    send_request(&client, "PUT", "/boot-source", BootSource {
        kernel_image_path: KERNEL_PATH.to_string(),
        boot_args: "console=ttyS0 reboot=k panic=1 pci=off ip=172.16.0.2::172.16.0.1:255.255.255.0::eth0:off root=/dev/vda rw".to_string(),
    }).await?;
    
    send_request(&client, "PUT", "/drives/rootfs", Drive {
        drive_id: "rootfs".to_string(), path_on_host: ROOTFS_PATH.to_string(), is_root_device: true, is_read_only: false,
    }).await?;
    
    send_request(&client, "PUT", "/vsock", Vsock { guest_cid: 3, uds_path: VSOCK_PATH.to_string() }).await?;


    // 6. BOOT
    println!("🔥 BOOTING... (Login as root/root, run 'python3 agent.py &')\r");
    send_request(&client, "PUT", "/actions", Action { action_type: "InstanceStart".to_string() }).await?;

    // 7. COUNTDOWN (60 seconds)
    // A. Wait for Boot (20 seconds for the VM to reach login screen)
    println!("⏳ Waiting 20s for login prompt...");
    thread::sleep(time::Duration::from_secs(30));

    // B. The Robot Typer
    // .take() grabs the stdin handle so we can use it
    if let Some(mut vm_stdin) = child.stdin.take() {
        println!("🤖 ROBOT: Typing credentials...");
        
        vm_stdin.write_all(b"root\n")?;        // Username
        thread::sleep(time::Duration::from_secs(5)); // Small pause

        vm_stdin.write_all(b"root\n")?;        // Password
        thread::sleep(time::Duration::from_secs(5));

        println!("🤖 ROBOT: Starting Agent...");
        vm_stdin.write_all(b"python3 agent.py &\n")?; // Command
    }

    // C. Wait for Agent (5 seconds for Python to start listening)
    println!("⏳ Waiting 5s for agent to warm up...");
    thread::sleep(time::Duration::from_secs(5));

    // 8. PAUSE & SNAPSHOT
    println!("\r\n⏸️  PAUSING VM...                                       \r");
    send_request(&client, "PATCH", "/vm", VmState { state: "Paused".to_string() }).await?;

    println!("📸 SAVING SNAPSHOT...                                    \r");
    let cwd = std::env::current_dir()?;
    let snap_abs = cwd.join(SNAPSHOT_PATH).to_str().unwrap().to_string();
    let mem_abs = cwd.join(MEM_PATH).to_str().unwrap().to_string();

    send_request(&client, "PUT", "/snapshot/create", SnapshotConfig {
        snapshot_type: "Full".to_string(),
        snapshot_path: snap_abs,
        mem_file_path: mem_abs,
    }).await?;

    println!("✅ SNAPSHOT COMPLETE!                                    \r");
    
    // 9. Restore & Exit
    child.kill()?;
    Ok(())
}

async fn send_request<T: Serialize>(
    client: &hyper_util::client::legacy::Client<hyperlocal::UnixConnector, http_body_util::Full<hyper::body::Bytes>>,
    method: &str, endpoint: &str, body: T
) -> Result<(), Box<dyn std::error::Error>> {
    let uri: hyper::Uri = hyperlocal::Uri::new(API_SOCKET, endpoint).into();
    let json = serde_json::to_string(&body)?;
    let req_method = match method { "PUT" => hyper::Method::PUT, "PATCH" => hyper::Method::PATCH, _ => hyper::Method::GET };
    let req = hyper::Request::builder().method(req_method).uri(uri)
        .header("Content-Type", "application/json").body(http_body_util::Full::new(hyper::body::Bytes::from(json)))?;
    let res = client.request(req).await?;
    if !res.status().is_success() {
        let body = http_body_util::BodyExt::collect(res.into_body()).await?.to_bytes();
        panic!("\r\n❌ API ERROR: {:?}\r\n", String::from_utf8(body.to_vec()));
    }
    Ok(())
}