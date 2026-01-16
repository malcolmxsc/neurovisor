use std::process::{Command, Stdio};
use std::path::Path;
use std::time::Duration;
use std::thread;

// --- HYPER 1.0 IMPORTS ---
use hyper::{Method, Request, body::Bytes};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use hyperlocal::{UnixConnector, Uri};
use http_body_util::Full;
use serde_json::json;

#[tokio::main]
async fn main() {
    let socket_path = "/tmp/firecracker.socket";
    let firecracker_bin = "./firecracker";
    let kernel_path = "./vmlinux";
    let rootfs_path = "./rootfs.ext4";

    // 1. Cleanup Old Socket
    if Path::new(socket_path).exists() {
        std::fs::remove_file(socket_path).expect("Failed to remove old socket");
    }

    println!("🚀 Launching Firecracker...");
    
    // 2. Spawn Firecracker
    let mut child = Command::new(firecracker_bin)
        .arg("--api-sock")
        .arg(socket_path)
        .stdout(Stdio::inherit()) 
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to start Firecracker");

    // 3. Wait for Socket
    let mut ready = false;
    for _ in 0..20 {
        if Path::new(socket_path).exists() {
            ready = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    if !ready {
        panic!("Firecracker socket failed to appear!");
    }

    let client = Client::builder(TokioExecutor::new()).build(UnixConnector);

    // --- STEP 1: CONFIGURE BOOT SOURCE ---
    println!("⚙️  Configuring Boot Source (Kernel)...");
    let uri_boot: hyper::Uri = Uri::new(socket_path, "/boot-source").into();
    let boot_config = json!({
        "kernel_image_path": kernel_path,
        "boot_args": "console=ttyS0 reboot=k panic=1 pci=off ip=172.16.0.2::172.16.0.1:255.255.255.0::eth0:off"
    });
    let req = Request::builder()
        .method(Method::PUT)
        .uri(uri_boot)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(boot_config.to_string())))
        .unwrap();
    client.request(req).await.expect("Failed to configure boot source");

    // --- STEP 2: CONFIGURE NETWORK (Must be before Boot) ---
    println!("🔌 Configuring Network Interface...");
    let uri_net: hyper::Uri = Uri::new(socket_path, "/network-interfaces/eth0").into();
    let net_config = json!({
        "iface_id": "eth0",
        "guest_mac": "AA:FC:00:00:00:01",
        "host_dev_name": "tap0"
    });
    let req = Request::builder()
        .method(Method::PUT)
        .uri(uri_net)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(net_config.to_string())))
        .unwrap();
    client.request(req).await.expect("Failed to configure network");

    // --- STEP 3: ATTACH DRIVE ---
    println!("💾 Attaching Root Filesystem...");
    let uri_drive: hyper::Uri = Uri::new(socket_path, "/drives/rootfs").into();
    let drive_config = json!({
        "drive_id": "rootfs",
        "path_on_host": rootfs_path,
        "is_root_device": true,
        "is_read_only": false
    });
    let req = Request::builder()
        .method(Method::PUT)
        .uri(uri_drive)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(drive_config.to_string())))
        .unwrap();
    client.request(req).await.expect("Failed to attach drive");

    // --- STEP 4: START INSTANCE ---
    println!("🔥 BOOTING INSTANCE...");
    let uri_action: hyper::Uri = Uri::new(socket_path, "/actions").into();
    let action_config = json!({
        "action_type": "InstanceStart"
    });
    let req = Request::builder()
        .method(Method::PUT)
        .uri(uri_action)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(action_config.to_string())))
        .unwrap();
    client.request(req).await.expect("Failed to start instance");

    println!("--------------------------------------------------");
    println!("       VM IS RUNNING (Press Ctrl+C to exit)       ");
    println!("--------------------------------------------------");
    
    let _ = child.wait();
}