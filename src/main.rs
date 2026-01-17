use std::process::{Command, Stdio};
use std::{thread, time};
use std::path::Path;
use serde::Serialize;
// use std::os::unix::net::UnixStream; 
// use tokio::io::{AsyncReadExt, AsyncWriteExt}; 
use nix::sys::termios::{tcgetattr, tcsetattr, SetArg, LocalFlags};

// --- API structs ---
#[derive(Serialize)]
struct BootSource {
    kernel_image_path: String,
    boot_args: String,
}

#[derive(Serialize)]
struct Drive {
    drive_id: String,
    path_on_host: String,
    is_root_device: bool,
    is_read_only: bool,
}

#[derive(Serialize)]
struct NetworkInterface {
    iface_id: String,
    guest_mac: String,
    host_dev_name: String,
}

#[derive(Serialize)]
struct Vsock {
    guest_cid: u32,
    uds_path: String,
}

#[derive(Serialize)]
struct Action {
    action_type: String,
}

// --- Constants ---
const FIRECRACKER_BIN: &str = "./firecracker";
const API_SOCKET: &str = "/tmp/firecracker.socket";
const KERNEL_PATH: &str = "./vmlinux";
const ROOTFS_PATH: &str = "./rootfs.ext4";
const VSOCK_PATH: &str = "./neurovisor.vsock";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 Launching NeuroVisor (Hybrid Mode)...");

    // 1. Clean up previous sockets
    let _ = std::fs::remove_file(API_SOCKET);
    let _ = std::fs::remove_file(VSOCK_PATH);

    // 2. Spawn Firecracker
    let mut child = Command::new(FIRECRACKER_BIN)
        .arg("--api-sock")
        .arg(API_SOCKET)
        .stdin(Stdio::inherit()) 
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    // 3. Wait for API socket
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build(hyperlocal::UnixConnector);
    
    let mut retries = 0;
    while !Path::new(API_SOCKET).exists() {
        if retries > 20 { panic!("Firecracker failed to start!"); }
        thread::sleep(time::Duration::from_millis(100));
        retries += 1;
    }

    // 4. Configure Kernel
    println!("⚙️  Configuring Boot Source...");
    let boot_config = BootSource {
        kernel_image_path: KERNEL_PATH.to_string(),
        boot_args: "console=ttyS0 reboot=k panic=1 pci=off ip=172.16.0.2::172.16.0.1:255.255.255.0::eth0:off root=/dev/vda rw virtio_mmio.device=4K@0xd0000000:5".to_string(),
    };
    // FIX: Added leading slash
    send_request(&client, "/boot-source", boot_config).await?;

    // 5. Configure Network
    println!("🔌 Configuring Network...");
    let net_config = NetworkInterface {
        iface_id: "eth0".to_string(),
        guest_mac: "AA:FC:00:00:00:01".to_string(),
        host_dev_name: "tap0".to_string(),
    };
    // FIX: Added leading slash
    send_request(&client, "/network-interfaces/eth0", net_config).await?;

    // 6. Configure RootFS
    println!("💾 Attaching Root Filesystem...");
    let drive_config = Drive {
        drive_id: "rootfs".to_string(),
        path_on_host: ROOTFS_PATH.to_string(),
        is_root_device: true,
        is_read_only: false,
    };
    // FIX: Added leading slash
    send_request(&client, "/drives/rootfs", drive_config).await?;

    // 7. Configure Vsock
    println!("🔗 Creating Vsock Wormhole...");
    let vsock_config = Vsock {
        guest_cid: 3,
        uds_path: VSOCK_PATH.to_string(),
    };
    // FIX: Added leading slash
    send_request(&client, "/vsock", vsock_config).await?;

    // 8. Start Instance
    println!("🔥 BOOTING INSTANCE...");
    let action = Action { action_type: "InstanceStart".to_string() };
    // FIX: Added leading slash
    send_request(&client, "/actions", action).await?;

    println!("--------------------------------------------------");
    println!("       VM IS RUNNING (Type 'reboot' to exit)      ");
    println!("--------------------------------------------------");

    // 9. Interactive Mode
    let stdin = std::io::stdin();
    let saved_termios = tcgetattr(&stdin)?;
    let mut raw_termios = saved_termios.clone();
    raw_termios.local_flags.remove(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG);
    tcsetattr(&stdin, SetArg::TCSADRAIN, &raw_termios)?;

    let _ = child.wait();

    tcsetattr(&stdin, SetArg::TCSADRAIN, &saved_termios)?;
    println!("\n🛑 VM Exited.");

    Ok(())
}

async fn send_request<T: Serialize>(
    client: &hyper_util::client::legacy::Client<hyperlocal::UnixConnector, http_body_util::Full<hyper::body::Bytes>>,
    endpoint: &str,
    body: T
) -> Result<(), Box<dyn std::error::Error>> {
    let uri: hyper::Uri = hyperlocal::Uri::new(API_SOCKET, endpoint).into();
    let json = serde_json::to_string(&body)?;
    let req = hyper::Request::builder()
        .method(hyper::Method::PUT)
        .uri(uri)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(http_body_util::Full::new(hyper::body::Bytes::from(json)))?;

    let res = client.request(req).await?;
    let status = res.status();
    
    if !status.is_success() {
        let body_bytes = http_body_util::BodyExt::collect(res.into_body()).await?.to_bytes();
        let error_msg = String::from_utf8(body_bytes.to_vec())?;
        println!("❌ API Error on {}: {} - {}", endpoint, status, error_msg);
    }

    Ok(())
}