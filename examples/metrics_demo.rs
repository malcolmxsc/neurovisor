//! Prometheus Metrics Demo
//!
//! This example demonstrates the metrics collection and export capabilities:
//! 1. Starts a gRPC server with metrics instrumentation
//! 2. Makes inference requests to generate metrics
//! 3. Displays the Prometheus metrics output
//!
//! Run with: cargo run --example metrics_demo
//! (Requires Ollama running on localhost:11434)

use std::sync::Arc;

use neurovisor::grpc::inference::inference_service_client::InferenceServiceClient;
use neurovisor::grpc::inference::inference_service_server::InferenceServiceServer;
use neurovisor::grpc::inference::InferenceRequest;
use neurovisor::grpc::server::InferenceServer;
use neurovisor::metrics::{encode_metrics, CGROUP_CPU_THROTTLED, CGROUP_MEMORY_USAGE};
use neurovisor::ollama::OllamaClient;
use neurovisor::security::RateLimiter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("┌─────────────────────────────────────────┐");
    println!("│  Prometheus Metrics Demo                │");
    println!("└─────────────────────────────────────────┘\n");

    // Start gRPC server
    let addr = "127.0.0.1:50052";
    println!("1. Starting gRPC server on {}...", addr);

    let ollama = OllamaClient::new("http://localhost:11434");
    let rate_limiter = Arc::new(RateLimiter::new(100, 50.0));
    let inference_server = InferenceServer::new(ollama, rate_limiter);
    let service = InferenceServiceServer::new(inference_server);

    let server_handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(service)
            .serve(addr.parse().unwrap())
            .await
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    println!("   ✅ Server started\n");

    // Make some requests to generate metrics
    println!("2. Making inference requests to generate metrics...");
    let mut client = InferenceServiceClient::connect("http://127.0.0.1:50052").await?;

    // Make 3 requests
    for i in 1..=3 {
        print!("   Request {}/3... ", i);
        let request = tonic::Request::new(InferenceRequest {
            prompt: format!("Say the number {} and nothing else.", i),
            model: "llama3.2".to_string(),
            temperature: 0.0,
            max_tokens: 10,
            stream: false,
            metadata: Default::default(),
        });

        match client.infer(request).await {
            Ok(response) => {
                let r = response.into_inner();
                println!("✅ {} tokens, {:.1}ms", r.tokens_generated, r.latency_ms);
            }
            Err(e) => {
                println!("❌ {}", e);
            }
        }
    }

    // Simulate cgroup metrics (in production, these would come from CgroupManager)
    println!("\n3. Simulating cgroup metrics for demo...");
    CGROUP_MEMORY_USAGE.with_label_values(&["vm-demo"]).set(256.0 * 1024.0 * 1024.0); // 256MB
    CGROUP_CPU_THROTTLED.with_label_values(&["vm-demo"]).inc_by(5.0);
    println!("   ✅ Set cgroup_memory_usage_bytes{{vm=\"vm-demo\"}} = 268435456");
    println!("   ✅ Set cgroup_cpu_throttled_total{{vm=\"vm-demo\"}} = 5");

    // Export and display metrics
    println!("\n4. Prometheus Metrics Output:");
    println!("─────────────────────────────────────────────────────────────");

    let metrics = encode_metrics();

    // Filter to show only neurovisor metrics (skip comments for cleaner output)
    for line in metrics.lines() {
        if line.starts_with("neurovisor_") {
            println!("{}", line);
        }
    }

    println!("─────────────────────────────────────────────────────────────");

    // Show summary
    println!("\n5. Metrics Summary:");
    println!("┌────────────────────────────────────────────────────────────┐");
    println!("│ Metric                              │ Description          │");
    println!("├────────────────────────────────────────────────────────────┤");
    println!("│ neurovisor_requests_total           │ Total requests       │");
    println!("│ neurovisor_tokens_generated_total   │ Tokens generated     │");
    println!("│ neurovisor_inference_duration_*     │ Ollama latency       │");
    println!("│ neurovisor_grpc_request_duration_*  │ End-to-end latency   │");
    println!("│ neurovisor_requests_in_flight       │ Concurrent requests  │");
    println!("│ neurovisor_request_size_bytes_*     │ Prompt sizes         │");
    println!("│ neurovisor_errors_total             │ Error count          │");
    println!("│ neurovisor_cgroup_memory_usage_*    │ VM memory usage      │");
    println!("│ neurovisor_cgroup_cpu_throttled_*   │ CPU throttle events  │");
    println!("└────────────────────────────────────────────────────────────┘");

    // Cleanup
    server_handle.abort();

    println!("\n┌─────────────────────────────────────────┐");
    println!("│  ✅ Metrics demo complete!              │");
    println!("└─────────────────────────────────────────┘");

    Ok(())
}
