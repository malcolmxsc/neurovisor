//! NeuroVisor Daemon - Multi-VM Pool Orchestrator
//!
//! This daemon maintains a pool of pre-warmed Firecracker VMs ready for instant
//! assignment to inference requests. It runs as a long-lived process with:
//!
//! - Pre-warmed VM pool (configurable size)
//! - Gateway gRPC server for external requests
//! - Background pool replenisher
//! - Prometheus metrics endpoint
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

use neurovisor::vm::{VMManager, VMManagerConfig, VMPool};
use neurovisor::ollama::OllamaClient;
use neurovisor::grpc::GatewayServer;
use neurovisor::grpc::inference::inference_service_server::InferenceServiceServer;
use neurovisor::cgroups::ResourceLimits;
use neurovisor::security::RateLimiter;
use neurovisor::metrics::encode_metrics;

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
}

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();

    let use_snapshot = args.iter().any(|a| a == "--snapshot" || a == "-s");

    let warm_size = args.iter()
        .position(|a| a == "--warm")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_WARM_SIZE);

    let max_size = args.iter()
        .position(|a| a == "--max")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_SIZE);

    Args {
        use_snapshot,
        warm_size,
        max_size,
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
    println!("[INFO] ✅ VM POOL READY (warm: {}, max: {})", args.warm_size, args.max_size);

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
