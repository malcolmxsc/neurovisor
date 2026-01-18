use std::process::{Command, Stdio};
use std::{thread, time};
use std::path::Path;
use serde::Serialize;
use tokio::net::UnixStream; 
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Serialize)]
struct SnapshotLoad { 
    snapshot_path: String, 
    mem_file_path: String, 
    resume_vm: bool 
}

const FIRECRACKER_BIN: &str = "./firecracker";
const API_SOCKET: &str = "/tmp/firecracker.socket";
const MEM_PATH: &str = "./mem_file";
const VSOCK_PATH: &str = "./neurovisor.vsock";
const SNAPSHOT_PATH: &str = "./snapshot_file";


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("📸 STARTING SNAPSHOT BUILDER...");

    // 1. AGGRESSIVE CLEANUP (Fixes the "File Exists" bug)
    let _ = std::fs::remove_file(API_SOCKET);
    let _ = std::fs::remove_file(VSOCK_PATH);
    
    // 2. Launch Firecracker
    let mut child = Command::new(FIRECRACKER_BIN).arg("--api-sock").arg(API_SOCKET)
        .stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).spawn()?;

    // 3. Wait for API
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build(hyperlocal::UnixConnector);
    while !Path::new(API_SOCKET).exists() { thread::sleep(time::Duration::from_millis(100)); }


    // 4. Configure VM new way for snapshot loading
    let cwd = std::env::current_dir()?;
    let snap_abs = cwd.join(SNAPSHOT_PATH).to_str().unwrap().to_string();
    let mem_abs = cwd.join(MEM_PATH).to_str().unwrap().to_string();

    println!("⚡ RESTORING VM STATE...");

    // Send the snapshot command using the new struct
    send_request(&client, "PUT", "/snapshot/load", SnapshotLoad {
        snapshot_path: snap_abs,
        mem_file_path: mem_abs,
        resume_vm: true, // This tells the CPU to wake up immediately
    }).await?;



    // 5. CONNECT TO AGENT (The New Logic)
    println!("📞 Dialing Agent...");
    
    // Wait for the Vsock file to appear
    while !Path::new(VSOCK_PATH).exists() { 
        thread::sleep(time::Duration::from_millis(1)); 
    }

    // Connect to the Python Agent
    match UnixStream::connect(VSOCK_PATH).await {
        Ok(mut stream) => {
            println!("✅ Connected to Agent!");

            // Handshake
            stream.write_all(b"CONNECT 5000\n").await?;
            
            // Send Data
            println!("📤 Sending Payload: 'Hello from the Future'");
            stream.write_all(b"Hello from the Future").await?;

            // Read Response
            let mut buffer = [0; 1024];
            let n = stream.read(&mut buffer).await?;
            let response = String::from_utf8_lossy(&buffer[..n]);
            
            println!("--------------------------------------------------");
            println!("📩 AGENT RESPONSE: {}", response);
            println!("--------------------------------------------------");
        }
        Err(e) => {
            println!("❌ Connection Failed: {}", e);
        }
    }
    child.kill()?;
    Ok(())
}

// Unified Request Handler with Error Checking
async fn send_request<T: Serialize>(
    client: &hyper_util::client::legacy::Client<hyperlocal::UnixConnector, http_body_util::Full<hyper::body::Bytes>>,
    method: &str,
    endpoint: &str, 
    body: T
) -> Result<(), Box<dyn std::error::Error>> {
    let uri: hyper::Uri = hyperlocal::Uri::new(API_SOCKET, endpoint).into();
    let json = serde_json::to_string(&body)?;
    
    let req_method = match method {
        "PUT" => hyper::Method::PUT,
        "PATCH" => hyper::Method::PATCH,
        _ => hyper::Method::GET,
    };

    let req = hyper::Request::builder()
        .method(req_method)
        .uri(uri)
        .header("Content-Type", "application/json")
        .body(http_body_util::Full::new(hyper::body::Bytes::from(json)))?;

    let res = client.request(req).await?;
    let status = res.status();

    if !status.is_success() {
        // If Firecracker complains, we print the error and CRASH immediately
        // This prevents "Silent Failures"
        let body_bytes = http_body_util::BodyExt::collect(res.into_body()).await?.to_bytes();
        let error_msg = String::from_utf8(body_bytes.to_vec())?;
        
        // Panic will print the error clearly to the console
        panic!("\r\n❌ API ERROR on {}: {} - {}\r\n", endpoint, status, error_msg);
    }

    Ok(())
}