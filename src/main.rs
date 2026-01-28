use std::process::Stdio;

// Import our new modules
use neurovisor::vm::{spawn_firecracker, wait_for_api_socket, FirecrackerClient, to_absolute_path};
use neurovisor::ollama::OllamaClient;
use neurovisor::grpc::server::InferenceServer;
use neurovisor::grpc::inference::inference_service_server::InferenceServiceServer;

// In production, these would be loaded from a .env or config file
const API_SOCKET: &str = "/tmp/firecracker.socket";
const KERNEL_PATH: &str = "./vmlinuz";
const ROOTFS_PATH: &str = "./rootfs.ext4";
const VSOCK_PATH: &str = "./neurovisor.vsock";
const VSOCK_PORT: u32 = 6000;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Production Standard: Use structured logging
    println!("[INFO] 📸 INITIALIZING NEUROVISOR ORCHESTRATOR...");

    // 1. CLEANUP
    let _ = std::fs::remove_file(API_SOCKET);
    let _ = std::fs::remove_file(VSOCK_PATH);

    // 2. LAUNCH VMM
    let mut child = spawn_firecracker(API_SOCKET, Stdio::inherit())?;

    // 3. WAIT FOR API
    wait_for_api_socket(API_SOCKET, None)?;

    // 4. CREATE FIRECRACKER CLIENT
    let fc_client = FirecrackerClient::new(API_SOCKET);

    // 5. CONFIGURE VM: KERNEL, ROOTFS, AND VSOCK
    let kernel_abs = to_absolute_path(KERNEL_PATH)?;
    let rootfs_abs = to_absolute_path(ROOTFS_PATH)?;

    println!("[INFO] 🔧 CONFIGURING VM BOOT...");
    fc_client.boot_source(
        &kernel_abs,
        "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/usr/local/bin/run_guest.sh",
    ).await?;

    println!("[INFO] 💾 ADDING ROOT DRIVE: {}", ROOTFS_PATH);
    fc_client.add_drive("root", &rootfs_abs, true, false).await?;

    println!("[INFO] 🔌 CONFIGURING VSOCK");
    fc_client.configure_vsock(3, VSOCK_PATH).await?;

    // Set up gRPC server BEFORE starting VM (guest will connect immediately after boot)
    let ollama = OllamaClient::new("http://localhost:11434");
    let inference_server = InferenceServer::new(ollama);
    let service = InferenceServiceServer::new(inference_server);

    // Firecracker convention: guest connects to CID 2 port P -> host socket at {uds_path}_{P}
    let vsock_listener_path = format!("{}_{}", VSOCK_PATH, VSOCK_PORT);
    println!("[INFO] 🚀 STARTING GRPC SERVER ON {} ...", vsock_listener_path);

    // Remove any existing socket file
    let _ = std::fs::remove_file(&vsock_listener_path);

    let listener = tokio::net::UnixListener::bind(&vsock_listener_path)?;

    // Spawn gRPC server in background task
    let grpc_handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
            .await
    });

    // NOW start the VM (gRPC server is already listening)
    println!("[INFO] ⚡ STARTING VM");
    fc_client.start().await?;

    println!("[INFO] ⏳ WAITING FOR VM TO COMPLETE...");

    // Wait for Firecracker process to exit (VM will poweroff after guest_client completes)
    let status = child.wait()?;
    println!("[INFO] 🛑 VM EXITED WITH STATUS: {:?}", status);

    // Abort the gRPC server task (it would block forever otherwise)
    grpc_handle.abort();
    println!("[INFO] 🛑 ORCHESTRATOR EXIT CLEAN");
    Ok(())
}