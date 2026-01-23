use std::process::Stdio;
use std::path::Path;
use tokio::net::UnixStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{timeout, Duration};
use futures_util::StreamExt;

// Import our new modules
use neurovisor::vm::{spawn_firecracker, wait_for_api_socket, FirecrackerClient, to_absolute_path};
use neurovisor::ollama::OllamaClient;

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

    // 5. CONNECT TO AGENT WITH RETRY LOGIC
    println!("[INFO] 📞 DIALING VSOCK AGENT...");
    
    let execution_timeout = Duration::from_secs(5);
    
    let result = timeout(execution_timeout, async {
        // PRODUCTION PATTERN: Retry connection up to 5 times
        let mut stream = None;
        for i in 0..5 {
            if Path::new(VSOCK_PATH).exists() {
                if let Ok(s) = UnixStream::connect(VSOCK_PATH).await {
                    stream = Some(s);
                    break;
                }
            }
            println!("[DEBUG] Connection attempt {} failed, retrying...", i + 1);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let mut stream = stream.ok_or("Failed to connect after retries")?;
        println!("[INFO] ✅ VSOCK TUNNEL ESTABLISHED");

// --- NEW BLOCK (Corrected) ---
        // 1. Handshake Draining (Internal Firecracker Handshake)
        stream.write_all(b"CONNECT 6000\n").await?; 

        // 2. Smart Accumulator: Read until we find the JSON object
        let mut full_buffer = Vec::new();
        let mut temp_buffer = [0; 1024];

        // Keep reading from the stream until we see a closing brace '}'
        loop {
            let n = stream.read(&mut temp_buffer).await?;
            if n == 0 { break; } // Connection closed
            full_buffer.extend_from_slice(&temp_buffer[..n]);

            // Heuristic: If we have an opening '{' and closing '}', we probably have the JSON
            if full_buffer.contains(&b'{') && full_buffer.contains(&b'}') {
                break;
            }
        }

        // 3. Parse: Skip the "OK 1073741824" header and find the JSON
        // FIX: corrected 'fuller_buffer' to 'full_buffer' and 'inter()' to 'iter()'
        let json_start_index = full_buffer.iter().position(|&b| b == b'{')
            .ok_or("Failed to find JSON start '{' in guest stream")?;
            
        // FIX: corrected 'sserde_json' to 'serde_json'
        let guest_req: serde_json::Value = serde_json::from_slice(&full_buffer[json_start_index..])?;
        let prompt = guest_req["prompt"].as_str().unwrap_or("Hello");

        println!("[BRIDGE] Forwarding to Ollama: {}", prompt);

        // 4. Forward to Host Ollama API using our new client
        let ollama = OllamaClient::new("http://localhost:11434");
        let mut token_stream = ollama.generate_stream(prompt, "llama3.2").await?;

        // 5. Stream Tokens back to the Guest via Vsock
        let mut full_response = String::new();
        while let Some(token_result) = token_stream.next().await {
            match token_result {
                Ok(token) => {
                    if !token.is_empty() {
                        stream.write_all(token.as_bytes()).await?;
                        full_response.push_str(&token);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok::<String, Box<dyn std::error::Error + Send + Sync>>(full_response)
    }).await;

    // 6. TERMINATE & REPORT
    match result {
        Ok(Ok(response)) => {
            println!("--------------------------------------------------");
            println!("📩 AGENT RESPONSE:\n{}", response.trim());
            println!("--------------------------------------------------");
        }
        Ok(Err(e)) => println!("[ERROR] ❌ EXECUTION FAILED: {}", e),
        Err(_) => println!("[WARN] ⚠️ TIMEOUT: VM unresponsive after 5s"),
    }

    child.kill()?; // Ensure VM doesn't become a zombie
    println!("[INFO] 🛑 ORCHESTRATOR EXIT CLEAN");
    Ok(())
}