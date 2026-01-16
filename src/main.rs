use std::process::{Command, Stdio};
use std::path::Path;
use std::time::Duration;
use std::thread;

// --- HYPER 1.0 IMPORTS ---
use hyper::{Method, Request, body::Bytes};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use hyperlocal::{UnixConnector, Uri};
use http_body_util::Full; // <--- This is the new way to handle Bodies
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
    
    // 2. Spawn Firecracker (With logs visible)
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

    // 4. Create the HTTP Client
    // We use the 'legacy' client because it handles the connection pool logic for us
    let client = Client::builder(TokioExecutor::new()).build(UnixConnector);

    println!("⚙️  Configuring Boot Source (Kernel)...");
    let uri: hyper::Uri = Uri::new(socket_path, "/boot-source").into();
    let boot_config = json!({
        "kernel_image_path": kernel_path,
        "boot_args": "console=ttyS0 reboot=k panic=1 pci=off"
    });
    
    // FIX: Use Full::<Bytes>::new() instead of Body::from()
    let req = Request::builder()
        .method(Method::PUT)
        .uri(uri)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(boot_config.to_string()))) 
        .unwrap();
    client.request(req).await.expect("Failed to configure boot source");

    println!("💾 Attaching Root Filesystem...");
    let uri: hyper::Uri = Uri::new(socket_path, "/drives/rootfs").into();
    let drive_config = json!({
        "drive_id": "rootfs",
        "path_on_host": rootfs_path,
        "is_root_device": true,
        "is_read_only": false
    });
    let req = Request::builder()
        .method(Method::PUT)
        .uri(uri)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(drive_config.to_string())))
        .unwrap();
    client.request(req).await.expect("Failed to attach drive");

    println!("🔥 BOOTING INSTANCE...");
    let uri: hyper::Uri = Uri::new(socket_path, "/actions").into();
    let action_config = json!({
        "action_type": "InstanceStart"
    });
    let req = Request::builder()
        .method(Method::PUT)
        .uri(uri)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(action_config.to_string())))
        .unwrap();
    client.request(req).await.expect("Failed to start instance");

    println!("--------------------------------------------------");
    println!("       VM IS RUNNING (Press Ctrl+C to exit)       ");
    println!("--------------------------------------------------");
    
    // Wait for the child process to exit
    let _ = child.wait();
}