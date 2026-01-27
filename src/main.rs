use std::process::Stdio;

// Import our new modules
use neurovisor::vm::{spawn_firecracker, wait_for_api_socket, FirecrackerClient, to_absolute_path};
use neurovisor::ollama::OllamaClient;
use neurovisor::grpc::server::InferenceServer;
use neurovisor::grpc::inference::inference_service_server::InferenceServiceServer;

// In production, these would be loaded from a .env or config file
const API_SOCKET: &str = "/tmp/firecracker.socket";
const MEM_PATH: &str = "./mem_file";
const VSOCK_PATH: &str = "./neurovisor.vsock";
const SNAPSHOT_PATH: &str = "./snapshot_file";

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

    // 5. RESTORE STATE
    let snap_abs = to_absolute_path(SNAPSHOT_PATH)?;
    let mem_abs = to_absolute_path(MEM_PATH)?;

    println!("[INFO] ⚡ RESTORING SNAPSHOT: {}", SNAPSHOT_PATH);
    fc_client.load_snapshot(&snap_abs, &mem_abs, true).await?;

    let ollama = OllamaClient::new("http://localhost:11434");
    let inference_server = InferenceServer::new(ollama);
    let service = InferenceServiceServer::new(inference_server);
    // create Unix socket listener
    let listener = tokio::net::UnixListener::bind(VSOCK_PATH)?;

    // build and serve
    tonic::transport::Server::builder()
    .add_service(service)
    .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
    .await?;


    child.kill()?; // Ensure VM doesn't become a zombie
    println!("[INFO] 🛑 ORCHESTRATOR EXIT CLEAN");
    Ok(())
}