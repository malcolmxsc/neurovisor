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
//! # Run as daemon (default: 3 warm VMs, max 10, medium size)
//! sudo ./target/debug/neurovisor
//!
//! # Custom pool size
//! sudo ./target/debug/neurovisor --warm 5 --max 20
//!
//! # Use snapshots for faster VM boot
//! sudo ./target/debug/neurovisor --snapshot
//!
//! # Choose VM size tier (small/medium/large)
//! sudo ./target/debug/neurovisor --size large    # 4 CPU, 8GB RAM
//! sudo ./target/debug/neurovisor --size small    # 1 CPU, 2GB RAM
//!
//! # Run agent mode (single task, then exit)
//! sudo ./target/debug/neurovisor --agent "Find all prime numbers under 100"
//!
//! # Push metrics to Pushgateway (for batch/ephemeral jobs)
//! sudo ./target/debug/neurovisor --agent "task" --pushgateway http://localhost:9091
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
use neurovisor::cgroups::VMSize;
use neurovisor::grpc::inference::inference_service_server::InferenceServiceServer;
use neurovisor::grpc::GatewayServer;
use neurovisor::metrics::{encode_metrics, push_to_gateway};
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
    /// VM size tier (small, medium, large)
    vm_size: VMSize,
    /// Agent mode: run a single task and exit
    agent_task: Option<String>,
    /// LLM model to use (default: qwen3)
    model: String,
    /// Pushgateway URL for pushing metrics before exit (agent mode)
    pushgateway: Option<String>,
    /// OTLP endpoint for distributed tracing (default: http://localhost:4316)
    otlp_endpoint: Option<String>,
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

    let vm_size = args
        .iter()
        .position(|a| a == "--size")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<VMSize>().ok())
        .unwrap_or_default();

    let agent_task = args
        .iter()
        .position(|a| a == "--agent")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.to_string());

    let model = args
        .iter()
        .position(|a| a == "--model")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "qwen3".to_string());

    let pushgateway = args
        .iter()
        .position(|a| a == "--pushgateway")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.to_string());

    let otlp_endpoint = args
        .iter()
        .position(|a| a == "--otlp")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.to_string());

    Args {
        use_snapshot,
        warm_size,
        max_size,
        vm_size,
        agent_task,
        model,
        pushgateway,
        otlp_endpoint,
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
    println!("[INFO] VM size: {}", args.vm_size);
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
        resource_limits: args.vm_size.limits(),
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
        return run_agent_mode(
            task,
            Arc::clone(&pool),
            args.model,
            args.pushgateway,
            args.otlp_endpoint,
        ).await;
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
    println!("║  Gateway:   0.0.0.0:{}                                      ║", GATEWAY_PORT);
    println!("║  Metrics:   http://0.0.0.0:{}/metrics                       ║", METRICS_PORT);
    println!("║  VM Pool:   {} warm, {} max                                    ║", args.warm_size, args.max_size);
    println!("║  VM Size:   {:<47}║", format!("{}", args.vm_size));
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();
    println!("[INFO] Press Ctrl+C to shutdown gracefully");

    // ─────────────────────────────────────────────────────────────────────────
    // 8. WAIT FOR SHUTDOWN SIGNAL
    // ─────────────────────────────────────────────────────────────────────────
    tokio::signal::ctrl_c().await?;

    println!();
    println!("[INFO] 🛑 SHUTDOWN SIGNAL RECEIVED");

    // ─────────────────────────────────────────────────────────────────────────
    // 9. GRACEFUL SHUTDOWN
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
    model: String,
    pushgateway: Option<String>,
    otlp_endpoint: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialize OpenTelemetry tracing
    if let Err(e) = neurovisor::tracing::init_tracing("neurovisor", otlp_endpoint.as_deref()) {
        eprintln!("[WARN] Failed to initialize tracing: {}", e);
        eprintln!("[WARN] Traces will not be exported to Tempo");
    } else {
        println!("[INFO] ✅ TRACING INITIALIZED (OTLP endpoint: {})",
            otlp_endpoint.as_deref().unwrap_or("http://localhost:4316"));
    }

    println!();
    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║                    NEUROVISOR AGENT MODE                       ║");
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();
    println!("[AGENT] Task: {}", task);
    println!("[AGENT] Model: {}", model);
    println!();

    // Create chat client for Ollama
    let chat_client = ChatClient::new("http://localhost:11434");

    // Create agent controller with specified model
    let config = AgentConfig {
        model,
        ..AgentConfig::default()
    };
    let controller = AgentController::new(chat_client, Arc::clone(&pool), config);

    // Run the agent
    let trace_id = match controller.run(&task).await {
        Ok(result) => {
            println!();
            println!("╔════════════════════════════════════════════════════════════════╗");
            println!("║                      TASK COMPLETED                            ║");
            println!("╚════════════════════════════════════════════════════════════════╝");
            println!();
            println!("[AGENT] Iterations: {}", result.iterations);
            println!("[AGENT] Tool calls: {}", result.tool_calls_made);
            println!("[AGENT] Trace ID: {}", result.trace_id);
            if let Some(load_time) = result.model_load_time_ms {
                println!("[AGENT] Model load time: {:.2}ms ({:.2}s)", load_time, load_time / 1000.0);
            }

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

            Some(result.trace_id)
        }
        Err(e) => {
            eprintln!();
            eprintln!("[AGENT] ERROR: {}", e);
            eprintln!();
            None
        }
    };

    // Push metrics to Pushgateway if configured
    if let Some(gateway_url) = pushgateway {
        println!("[INFO] Pushing metrics to Pushgateway at {}...", gateway_url);
        match push_to_gateway(&gateway_url, "neurovisor_agent", trace_id.as_deref()).await {
            Ok(()) => println!("[INFO] ✅ Metrics pushed to Pushgateway"),
            Err(e) => eprintln!("[WARN] Failed to push metrics: {}", e),
        }
    }

    // Cleanup
    println!("[INFO] Shutting down VM pool...");
    pool.shutdown().await;

    // Flush traces before exiting
    println!("[INFO] Flushing traces...");
    neurovisor::tracing::shutdown_tracing();

    println!("[INFO] ✅ AGENT MODE COMPLETE");

    Ok(())
}
