//! Test trace ID generation in gRPC responses
//!
//! This test:
//! 1. Starts a gRPC server on localhost:50051
//! 2. Makes an inference request
//! 3. Verifies the response contains a valid UUID v7 trace ID
//!
//! Run with: cargo run --bin test_trace_id
//! (Requires Ollama running on localhost:11434)

use neurovisor::ollama::OllamaClient;
use neurovisor::grpc::server::InferenceServer;
use neurovisor::grpc::inference::inference_service_server::InferenceServiceServer;
use neurovisor::grpc::inference::inference_service_client::InferenceServiceClient;
use neurovisor::grpc::inference::InferenceRequest;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("┌─────────────────────────────────────────┐");
    println!("│  Testing Trace ID Generation (UUID v7)  │");
    println!("└─────────────────────────────────────────┘\n");

    // Start gRPC server in background
    let addr = "127.0.0.1:50051";
    println!("1. Starting gRPC server on {}...", addr);

    let ollama = OllamaClient::new("http://localhost:11434");
    let inference_server = InferenceServer::new(ollama);
    let service = InferenceServiceServer::new(inference_server);

    let server_handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(service)
            .serve(addr.parse().unwrap())
            .await
    });

    // Give the server a moment to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    println!("   ✅ Server started\n");

    // Create gRPC client and make request
    println!("2. Making inference request...");
    let mut client = InferenceServiceClient::connect("http://127.0.0.1:50051").await?;

    let request = tonic::Request::new(InferenceRequest {
        prompt: "Say 'hello' and nothing else.".to_string(),
        model: "llama3.2".to_string(),
        temperature: 0.0,
        max_tokens: 10,
        stream: false,
        metadata: Default::default(),
    });

    let response = client.infer(request).await?;
    let inner = response.into_inner();

    println!("   ✅ Response received\n");

    // Display results
    println!("3. Response details:");
    println!("┌────────────────────────────────────────────────────────────┐");
    println!("│ Response:    {:>43} │", truncate(&inner.response, 43));
    println!("│ Tokens:      {:>43} │", inner.tokens_generated);
    println!("│ Latency:     {:>40.2} ms │", inner.latency_ms);
    println!("│ Model:       {:>43} │", inner.model_used);
    println!("│ Trace ID:    {:>43} │", inner.trace_id);
    println!("└────────────────────────────────────────────────────────────┘");

    // Validate trace ID format (UUID v7)
    println!("\n4. Validating trace ID...");
    let trace_id = &inner.trace_id;

    // UUID format: 8-4-4-4-12 hex chars = 36 total with hyphens
    let is_valid_format = trace_id.len() == 36
        && trace_id.chars().nth(8) == Some('-')
        && trace_id.chars().nth(13) == Some('-')
        && trace_id.chars().nth(18) == Some('-')
        && trace_id.chars().nth(23) == Some('-');

    // Check version digit (should be '7' for UUID v7)
    let version_char = trace_id.chars().nth(14);
    let is_v7 = version_char == Some('7');

    if is_valid_format && is_v7 {
        println!("   ✅ Valid UUID v7 format!");
        println!("   Version digit at position 14: '{}'", version_char.unwrap());

        // Extract timestamp from v7 UUID (first 12 hex chars = 48 bits of unix ms)
        let timestamp_hex = trace_id[0..8].to_owned() + &trace_id[9..13];
        if let Ok(timestamp_ms) = u64::from_str_radix(&timestamp_hex, 16) {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;

            let age_ms = now_ms.saturating_sub(timestamp_ms);
            println!("   Timestamp embedded: {} ms ago", age_ms);
        }
    } else {
        println!("   ❌ Invalid trace ID format!");
        println!("   Expected: UUID v7 (xxxxxxxx-xxxx-7xxx-xxxx-xxxxxxxxxxxx)");
        println!("   Got:      {}", trace_id);
    }

    // Cleanup
    server_handle.abort();

    println!("\n┌─────────────────────────────────────────┐");
    println!("│  ✅ Trace ID test complete!             │");
    println!("└─────────────────────────────────────────┘");

    Ok(())
}

fn truncate(s: &str, max_len: usize) -> String {
    let s = s.trim().replace('\n', " ");
    if s.len() <= max_len {
        s
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}
