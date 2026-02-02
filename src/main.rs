//! NeuroVisor Daemon - Multi-VM Pool Orchestrator
//!
//! This daemon maintains a pool of pre-warmed Firecracker VMs ready for instant
//! assignment to inference requests. It runs as a long-lived process with:
//!
//! - Pre-warmed VM pool (configurable size)
//! - Gateway gRPC server for external requests
//! - Background pool replenisher
//! - Prometheus metrics endpoint
//! - Agent mode for LLM-driven code execution
//!
//! # Usage
//!
//! ```bash
//! # Run as daemon (default: 3 warm VMs, max 10)
//! sudo ./target/debug/neurovisor
//!
//! # Custom pool size
//! sudo ./target/debug/neurovisor --warm 5 --max 20
//!
//! # Use snapshots for faster VM boot
//! sudo ./target/debug/neurovisor --snapshot
//!
//! # Run agent mode (single task, then exit)
//! sudo ./target/debug/neurovisor --agent "Find all prime numbers under 100"
//! ```

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use hyper::{body::Incoming, server::conn::http1, service::service_fn, Request, Response, Method, StatusCode};
use hyper_util::rt::TokioIo;
use http_body_util::Full;
use hyper::body::Bytes;
use tokio::net::TcpListener;

use neurovisor::agent::{AgentConfig, AgentController};
use neurovisor::cgroups::ResourceLimits;
use neurovisor::grpc::inference::inference_service_server::InferenceServiceServer;
use neurovisor::grpc::GatewayServer;
use neurovisor::metrics::encode_metrics;
use neurovisor::ollama::{ChatClient, OllamaClient};
use neurovisor::security::RateLimiter;
use neurovisor::vm::{VMManager, VMManagerConfig, VMPool};

// ─────────────────────────────────────────────────────────────────────────────
// Configuration Constants
// ─────────────────────────────────────────────────────────────────────────────

const KERNEL_PATH: &str = "./vmlinuz";
const ROOTFS_PATH: &str = "./rootfs.ext4";
const SNAPSHOT_PATH: &str = "./snapshot_file";
const MEM_PATH: &str = "./mem_file";
const METRICS_PORT: u16 = 9090;
const GATEWAY_PORT: u16 = 50051;
const VSOCK_PORT: u32 = 6000;

// Pool configuration defaults
const DEFAULT_WARM_SIZE: usize = 3;
const DEFAULT_MAX_SIZE: usize = 10;

fn snapshot_exists() -> bool {
    Path::new(SNAPSHOT_PATH).exists() && Path::new(MEM_PATH).exists()
}

/// Parse command line arguments
struct Args {
    use_snapshot: bool,
    warm_size: usize,
    max_size: usize,
    /// Agent mode: run a single task and exit
    agent_task: Option<String>,
}

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();

    let use_snapshot = args.iter().any(|a| a == "--snapshot" || a == "-s");

    let warm_size = args
        .iter()
        .position(|a| a == "--warm")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_WARM_SIZE);

    let max_size = args
        .iter()
        .position(|a| a == "--max")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_SIZE);

    let agent_task = args
        .iter()
        .position(|a| a == "--agent")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.to_string());

    Args {
        use_snapshot,
        warm_size,
        max_size,
        agent_task,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = parse_args();

    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║           NEUROVISOR DAEMON - Multi-VM Orchestrator            ║");
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();

    // Validate snapshot mode
    if args.use_snapshot && !snapshot_exists() {
        eprintln!("[ERROR] --snapshot requested but snapshot files not found");
        eprintln!("        Run `cargo run --bin builder` first to create a snapshot");
        std::process::exit(1);
    }

    let boot_mode = if args.use_snapshot { "snapshot" } else { "fresh" };
    println!("[INFO] Boot mode: {}", boot_mode);
    println!("[INFO] Pool config: warm={}, max={}", args.warm_size, args.max_size);
    println!();

    // ─────────────────────────────────────────────────────────────────────────
    // 1. START METRICS SERVER
    // ─────────────────────────────────────────────────────────────────────────
    let metrics_handle = tokio::spawn(async move {
        start_metrics_server(METRICS_PORT).await;
    });

    // ─────────────────────────────────────────────────────────────────────────
    // 2. CREATE VM MANAGER
    // ─────────────────────────────────────────────────────────────────────────
    let vm_config = VMManagerConfig {
        kernel_path: KERNEL_PATH.into(),
        rootfs_path: ROOTFS_PATH.into(),
        snapshot_path: if args.use_snapshot { Some(SNAPSHOT_PATH.into()) } else { None },
        mem_path: if args.use_snapshot { Some(MEM_PATH.into()) } else { None },
        resource_limits: ResourceLimits::medium(),
        vsock_port: VSOCK_PORT,
    };

    let vm_manager = Arc::new(VMManager::new(vm_config)?);
    println!("[INFO] ✅ VM MANAGER INITIALIZED");

    // ─────────────────────────────────────────────────────────────────────────
    // 3. CREATE AND INITIALIZE VM POOL
    // ─────────────────────────────────────────────────────────────────────────
    let pool = Arc::new(VMPool::new(
        Arc::clone(&vm_manager),
        args.warm_size,
        args.max_size,
    ));

    pool.initialize().await?;
    println!(
        "[INFO] ✅ VM POOL READY (warm: {}, max: {})",
        args.warm_size, args.max_size
    );

    // ─────────────────────────────────────────────────────────────────────────
    // AGENT MODE - Run a single task and exit
    // ─────────────────────────────────────────────────────────────────────────
    if let Some(task) = args.agent_task {
        return run_agent_mode(task, Arc::clone(&pool)).await;
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 4. START POOL REPLENISHER
    // ─────────────────────────────────────────────────────────────────────────
    let replenisher = VMPool::start_replenisher(Arc::clone(&pool));
    println!("[INFO] ✅ POOL REPLENISHER STARTED");

    // ─────────────────────────────────────────────────────────────────────────
    // 5. CREATE GATEWAY SERVER
    // ─────────────────────────────────────────────────────────────────────────
    let ollama = OllamaClient::new("http://localhost:11434");
    let rate_limiter = Arc::new(RateLimiter::new(100, 50.0));
    println!("[INFO] ✅ RATE LIMITER INITIALIZED (capacity: 100, rate: 50 req/sec)");

    let gateway = GatewayServer::new(Arc::clone(&pool), rate_limiter, ollama);
    let service = InferenceServiceServer::new(gateway);

    // ─────────────────────────────────────────────────────────────────────────
    // 6. START GATEWAY gRPC SERVER
    // ─────────────────────────────────────────────────────────────────────────
    let addr: SocketAddr = format!("0.0.0.0:{}", GATEWAY_PORT).parse()?;

    let grpc_handle = tokio::spawn(async move {
        println!("[INFO] ✅ GATEWAY LISTENING ON {}", addr);
        tonic::transport::Server::builder()
            .add_service(service)
            .serve(addr)
            .await
    });

    println!();
    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║                    NEUROVISOR DAEMON READY                     ║");
    println!("╠════════════════════════════════════════════════════════════════╣");
    println!("║  Gateway:  0.0.0.0:{}                                       ║", GATEWAY_PORT);
    println!("║  Metrics:  http://0.0.0.0:{}/metrics                        ║", METRICS_PORT);
    println!("║  VM Pool:  {} warm, {} max                                     ║", args.warm_size, args.max_size);
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();
    println!("[INFO] Press Ctrl+C to shutdown gracefully");

    // ─────────────────────────────────────────────────────────────────────────
    // 7. WAIT FOR SHUTDOWN SIGNAL
    // ─────────────────────────────────────────────────────────────────────────
    tokio::signal::ctrl_c().await?;

    println!();
    println!("[INFO] 🛑 SHUTDOWN SIGNAL RECEIVED");

    // ─────────────────────────────────────────────────────────────────────────
    // 8. GRACEFUL SHUTDOWN
    // ─────────────────────────────────────────────────────────────────────────
    println!("[INFO] Shutting down VM pool...");
    pool.shutdown().await;

    println!("[INFO] Stopping background tasks...");
    grpc_handle.abort();
    replenisher.abort();
    metrics_handle.abort();

    println!("[INFO] ✅ NEUROVISOR DAEMON STOPPED");
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Metrics Server
// ─────────────────────────────────────────────────────────────────────────────

/// Handle HTTP requests for the metrics endpoint
async fn handle_metrics_request(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/metrics") => {
            let metrics = encode_metrics();
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/plain; charset=utf-8")
                .body(Full::new(Bytes::from(metrics)))
                .unwrap())
        }
        (&Method::GET, "/health") => {
            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::from("OK")))
                .unwrap())
        }
        _ => {
            Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::from("Not Found")))
                .unwrap())
        }
    }
}

/// Start the Prometheus metrics HTTP server on the specified port.
async fn start_metrics_server(port: u16) {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[WARN] Failed to bind metrics server to port {}: {}", port, e);
            eprintln!("[WARN] Metrics endpoint will not be available");
            return;
        }
    };

    println!("[INFO] ✅ METRICS SERVER LISTENING ON http://0.0.0.0:{}/metrics", port);

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("[WARN] Failed to accept metrics connection: {}", e);
                continue;
            }
        };

        let io = TokioIo::new(stream);

        tokio::spawn(async move {
            if let Err(e) = http1::Builder::new()
                .serve_connection(io, service_fn(handle_metrics_request))
                .await
            {
                eprintln!("[WARN] Metrics connection error: {}", e);
            }
        });
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Agent Mode
// ─────────────────────────────────────────────────────────────────────────────

/// Run a single agent task and exit
async fn run_agent_mode(
    task: String,
    pool: Arc<VMPool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!();
    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║                    NEUROVISOR AGENT MODE                       ║");
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();
    println!("[AGENT] Task: {}", task);
    println!();

    // Create chat client for Ollama
    let chat_client = ChatClient::new("http://localhost:11434");

    // Create agent controller
    let config = AgentConfig::default();
    let controller = AgentController::new(chat_client, Arc::clone(&pool), config);

    // Run the agent
    match controller.run(&task).await {
        Ok(result) => {
            println!();
            println!("╔════════════════════════════════════════════════════════════════╗");
            println!("║                      TASK COMPLETED                            ║");
            println!("╚════════════════════════════════════════════════════════════════╝");
            println!();
            println!("[AGENT] Iterations: {}", result.iterations);
            println!("[AGENT] Tool calls: {}", result.tool_calls_made);
            println!("[AGENT] Trace ID: {}", result.trace_id);

            if !result.execution_records.is_empty() {
                println!();
                println!("── Execution History ──────────────────────────────────────────");
                for (i, record) in result.execution_records.iter().enumerate() {
                    println!();
                    println!("  [{}/{}] {} ({:.2}ms)", i + 1, result.execution_records.len(), record.language, record.duration_ms);
                    println!("  Exit code: {}{}", record.exit_code, if record.timed_out { " (timed out)" } else { "" });
                    if !record.stdout.is_empty() {
                        println!("  Stdout: {}", record.stdout.lines().next().unwrap_or(""));
                        if record.stdout.lines().count() > 1 {
                            println!("          ... ({} more lines)", record.stdout.lines().count() - 1);
                        }
                    }
                }
            }

            println!();
            println!("── Final Response ─────────────────────────────────────────────");
            println!();
            println!("{}", result.final_response);
            println!();
        }
        Err(e) => {
            eprintln!();
            eprintln!("[AGENT] ERROR: {}", e);
            eprintln!();
        }
    }

    // Cleanup
    println!("[INFO] Shutting down VM pool...");
    pool.shutdown().await;
    println!("[INFO] ✅ AGENT MODE COMPLETE");

    Ok(())
}
