use std::convert::Infallible;
use std::net::SocketAddr;
use std::process::Stdio;
use std::path::Path;
use std::sync::Arc;

use hyper::{body::Incoming, server::conn::http1, service::service_fn, Request, Response, Method, StatusCode};
use hyper_util::rt::TokioIo;
use http_body_util::Full;
use hyper::body::Bytes;
use tokio::net::TcpListener;
use tokio::time::{interval, Duration};

use neurovisor::vm::{spawn_firecracker, wait_for_api_socket, FirecrackerClient, to_absolute_path};
use neurovisor::ollama::OllamaClient;
use neurovisor::grpc::server::InferenceServer;
use neurovisor::grpc::inference::inference_service_server::InferenceServiceServer;
use neurovisor::cgroups::{CgroupManager, ResourceLimits};
use neurovisor::metrics::{encode_metrics, CGROUP_MEMORY_USAGE, CGROUP_CPU_THROTTLED};

const API_SOCKET: &str = "/tmp/firecracker.socket";
const KERNEL_PATH: &str = "./vmlinuz";
const ROOTFS_PATH: &str = "./rootfs.ext4";
const VSOCK_PATH: &str = "./neurovisor.vsock";
const VSOCK_PORT: u32 = 6000;
const SNAPSHOT_PATH: &str = "./snapshot_file";
const MEM_PATH: &str = "./mem_file";
const VM_ID: &str = "vm-1";
const METRICS_PORT: u16 = 9090;

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

    // 2. START METRICS SERVER (background task, starts immediately)
    let metrics_handle = tokio::spawn(async move {
        start_metrics_server(METRICS_PORT).await;
    });

    // 3. LAUNCH VMM (seccomp filter applied via pre_exec in child process)
    let mut child = spawn_firecracker(API_SOCKET, Stdio::inherit())?;
    let firecracker_pid = child.id();

    // 4. SET UP CGROUP RESOURCE LIMITS
    // Graceful degradation: if cgroup setup fails, log warning but continue
    // Wrapped in Arc to share between metrics collection and cleanup
    let cgroup_manager = Arc::new(setup_cgroup(VM_ID, firecracker_pid));

    // 5. WAIT FOR API
    wait_for_api_socket(API_SOCKET, None)?;

    // 6. CREATE FIRECRACKER CLIENT
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

    // 7. START CGROUP METRICS COLLECTION (if cgroups are active)
    let cgroup_metrics_handle = start_cgroup_metrics_collection(
        Arc::clone(&cgroup_manager),
        VM_ID.to_string(),
    );

    println!("[INFO] ⏳ WAITING FOR VM TO COMPLETE...");

    // Wait for Firecracker process to exit (VM will poweroff after guest_client completes)
    let status = child.wait()?;
    println!("[INFO] 🛑 VM EXITED WITH STATUS: {:?}", status);

    // Abort background server tasks (they would block forever otherwise)
    grpc_handle.abort();
    metrics_handle.abort();
    if let Some(handle) = cgroup_metrics_handle {
        handle.abort();
    }

    // Clean up cgroup if it was set up
    cleanup_cgroup(&cgroup_manager, VM_ID);

    println!("[INFO] 🛑 ORCHESTRATOR EXIT CLEAN");
    Ok(())
}

/// Set up cgroup for the Firecracker process with resource limits.
/// Returns Some(CgroupManager) on success, None if setup fails.
/// Failures are logged but don't stop the orchestrator.
fn setup_cgroup(vm_id: &str, pid: u32) -> Option<CgroupManager> {
    // Try to create the cgroup manager
    let manager = match CgroupManager::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[WARN] Failed to initialize cgroup manager: {}", e);
            eprintln!("[WARN] VM will run without resource limits");
            eprintln!("[WARN] (Requires root and cgroups v2)");
            return None;
        }
    };

    // Create cgroup with medium limits (2 cores, 4GB RAM)
    let limits = ResourceLimits::medium();
    if let Err(e) = manager.create(vm_id, limits.clone()) {
        eprintln!("[WARN] Failed to create cgroup '{}': {}", vm_id, e);
        eprintln!("[WARN] VM will run without resource limits");
        return None;
    }

    // Add Firecracker process to the cgroup
    if let Err(e) = manager.add_process(vm_id, pid) {
        eprintln!("[WARN] Failed to add PID {} to cgroup '{}': {}", pid, vm_id, e);
        eprintln!("[WARN] VM will run without resource limits");
        // Try to clean up the created cgroup
        let _ = manager.destroy(vm_id);
        return None;
    }

    println!("[INFO] ✅ CGROUP '{}' CREATED (limits: {} cores, {} GB RAM)",
        vm_id,
        limits.cpu_cores,
        limits.memory_bytes / (1024 * 1024 * 1024)
    );
    println!("[INFO]    Firecracker PID {} bound to cgroup", pid);

    Some(manager)
}

/// Clean up cgroup after VM exits
fn cleanup_cgroup(manager: &Arc<Option<CgroupManager>>, vm_id: &str) {
    if let Some(m) = manager.as_ref() {
        match m.destroy(vm_id) {
            Ok(()) => println!("[INFO] ✅ CGROUP '{}' DESTROYED", vm_id),
            Err(e) => eprintln!("[WARN] Failed to destroy cgroup '{}': {}", vm_id, e),
        }
    }
}

/// Start background task to collect cgroup metrics every 5 seconds.
/// Returns None if cgroups are not available.
fn start_cgroup_metrics_collection(
    manager: Arc<Option<CgroupManager>>,
    vm_id: String,
) -> Option<tokio::task::JoinHandle<()>> {
    // Only start collection if cgroups are active
    if manager.is_none() {
        return None;
    }

    println!("[INFO] ✅ CGROUP METRICS COLLECTION STARTED (interval: 5s)");

    let handle = tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(5));
        let mut last_throttled: u64 = 0;

        loop {
            interval.tick().await;

            if let Some(ref mgr) = *manager {
                // Collect memory usage
                match mgr.get_memory_usage(&vm_id) {
                    Ok(bytes) => {
                        CGROUP_MEMORY_USAGE.with_label_values(&[&vm_id]).set(bytes as f64);
                    }
                    Err(e) => {
                        eprintln!("[WARN] Failed to read cgroup memory: {}", e);
                    }
                }

                // Collect CPU stats (including throttle count)
                match mgr.get_cpu_stats(&vm_id) {
                    Ok(stats) => {
                        // CGROUP_CPU_THROTTLED is a counter, increment by delta
                        if stats.nr_throttled > last_throttled {
                            let delta = stats.nr_throttled - last_throttled;
                            CGROUP_CPU_THROTTLED.with_label_values(&[&vm_id]).inc_by(delta as f64);
                            last_throttled = stats.nr_throttled;
                        }
                    }
                    Err(e) => {
                        eprintln!("[WARN] Failed to read cgroup CPU stats: {}", e);
                    }
                }
            }
        }
    });

    Some(handle)
}

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
        _ => {
            Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::from("Not Found")))
                .unwrap())
        }
    }
}

/// Start the Prometheus metrics HTTP server on the specified port.
/// This runs as a background task and does not block the main VM lifecycle.
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