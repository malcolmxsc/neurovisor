use std::process::Stdio;
use std::path::Path;

use neurovisor::vm::{spawn_firecracker, wait_for_api_socket, FirecrackerClient, to_absolute_path};
use neurovisor::ollama::OllamaClient;
use neurovisor::grpc::server::InferenceServer;
use neurovisor::grpc::inference::inference_service_server::InferenceServiceServer;

const API_SOCKET: &str = "/tmp/firecracker.socket";
const KERNEL_PATH: &str = "./vmlinuz";
const ROOTFS_PATH: &str = "./rootfs.ext4";
const VSOCK_PATH: &str = "./neurovisor.vsock";
const VSOCK_PORT: u32 = 6000;
const SNAPSHOT_PATH: &str = "./snapshot_file";
const MEM_PATH: &str = "./mem_file";

fn snapshot_exists() -> bool {
    Path::new(SNAPSHOT_PATH).exists() && Path::new(MEM_PATH).exists()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let use_snapshot = std::env::args().any(|arg| arg == "--snapshot" || arg == "-s");
    let has_snapshot = snapshot_exists();

    if use_snapshot && !has_snapshot {
        eprintln!("[ERROR] --snapshot requested but snapshot files not found");
        eprintln!("        Run `cargo run --bin builder` first to create a snapshot");
        std::process::exit(1);
    }

    let mode = if use_snapshot && has_snapshot { "snapshot" } else { "fresh" };
    println!("[INFO] INITIALIZING NEUROVISOR ORCHESTRATOR (mode: {})...", mode);

    // 1. CLEANUP
    let _ = std::fs::remove_file(API_SOCKET);
    let _ = std::fs::remove_file(VSOCK_PATH);

    // 2. LAUNCH VMM
    let mut child = spawn_firecracker(API_SOCKET, Stdio::inherit())?;

    // 3. WAIT FOR API
    wait_for_api_socket(API_SOCKET, None)?;

    // 4. CREATE FIRECRACKER CLIENT
    let fc_client = FirecrackerClient::new(API_SOCKET);

    // Set up gRPC server BEFORE starting/resuming VM
    let ollama = OllamaClient::new("http://localhost:11434");
    let inference_server = InferenceServer::new(ollama);
    let service = InferenceServiceServer::new(inference_server);

    let vsock_listener_path = format!("{}_{}", VSOCK_PATH, VSOCK_PORT);
    println!("[INFO] STARTING GRPC SERVER ON {} ...", vsock_listener_path);

    let _ = std::fs::remove_file(&vsock_listener_path);
    let listener = tokio::net::UnixListener::bind(&vsock_listener_path)?;

    let grpc_handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
            .await
    });

    if use_snapshot && has_snapshot {
        // SNAPSHOT RESTORE PATH
        let snap_abs = to_absolute_path(SNAPSHOT_PATH)?;
        let mem_abs = to_absolute_path(MEM_PATH)?;

        println!("[INFO] LOADING SNAPSHOT...");
        fc_client.load_snapshot(&snap_abs, &mem_abs, false).await?;

        println!("[INFO] RESUMING VM...");
        fc_client.resume().await?;
    } else {
        // FRESH BOOT PATH
        let kernel_abs = to_absolute_path(KERNEL_PATH)?;
        let rootfs_abs = to_absolute_path(ROOTFS_PATH)?;

        println!("[INFO] CONFIGURING VM BOOT...");
        fc_client.boot_source(
            &kernel_abs,
            "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/usr/local/bin/run_guest.sh",
        ).await?;

        println!("[INFO] ADDING ROOT DRIVE: {}", ROOTFS_PATH);
        fc_client.add_drive("root", &rootfs_abs, true, false).await?;

        println!("[INFO] CONFIGURING VSOCK");
        fc_client.configure_vsock(3, VSOCK_PATH).await?;

        println!("[INFO] STARTING VM");
        fc_client.start().await?;
    }

    println!("[INFO] ⏳ WAITING FOR VM TO COMPLETE...");

    // Wait for Firecracker process to exit (VM will poweroff after guest_client completes)
    let status = child.wait()?;
    println!("[INFO] 🛑 VM EXITED WITH STATUS: {:?}", status);

    // Abort the gRPC server task (it would block forever otherwise)
    grpc_handle.abort();
    println!("[INFO] 🛑 ORCHESTRATOR EXIT CLEAN");
    Ok(())
}