//! Guest execution server - runs inside Firecracker VM
//!
//! Listens on vsock for code execution requests from the host.
//! Uses raw libc vsock calls for musl compatibility (tokio_vsock causes GPF in musl).

use std::collections::HashMap;
use std::os::unix::io::FromRawFd;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::{timeout, Duration};
use tonic::{Request, Response, Status};

// Include the generated proto code
pub mod execution {
    tonic::include_proto!("neurovisor.execution");
}

use execution::execute_chunk::Output;
use execution::execution_service_server::{ExecutionService, ExecutionServiceServer};
use execution::{ExecuteChunk, ExecuteMetadata, ExecuteRequest, ExecuteResponse};

const AF_VSOCK: libc::c_int = 40;
const SOCK_STREAM: libc::c_int = 1;
const VMADDR_CID_ANY: u32 = u32::MAX; // -1U, bind to any CID
const VSOCK_PORT: u32 = 6000;

#[repr(C)]
struct SockaddrVm {
    svm_family: u16,
    svm_reserved1: u16,
    svm_port: u32,
    svm_cid: u32,
    svm_flags: u8,
    svm_zero: [u8; 3],
}

/// Execution server implementing the gRPC service
pub struct ExecutionServer;

#[tonic::async_trait]
impl ExecutionService for ExecutionServer {
    async fn execute(
        &self,
        request: Request<ExecuteRequest>,
    ) -> Result<Response<ExecuteResponse>, Status> {
        let req = request.into_inner();
        let start = std::time::Instant::now();

        // Extract trace_id from environment if present
        let trace_id = req.env.get("NEUROVISOR_TRACE_ID")
            .map(|s| s.as_str())
            .unwrap_or("none");

        println!(
            "[GUEST] Execute request: language={}, code_len={}, trace_id={}",
            req.language,
            req.code.len(),
            trace_id
        );

        // Build command based on language
        // Note: Alpine uses BusyBox, so we use /bin/sh for shell commands
        // For compiled languages (Go, Rust), we write to temp file first
        let (program, args, temp_file): (String, Vec<String>, Option<String>) = match req.language.as_str() {
            "python" | "python3" => ("python3".to_string(), vec!["-c".to_string(), req.code.clone()], None),
            "bash" | "sh" | "shell" => ("/bin/sh".to_string(), vec!["-c".to_string(), req.code.clone()], None),
            "javascript" | "node" => ("node".to_string(), vec!["-e".to_string(), req.code.clone()], None),
            "go" | "golang" => {
                // Write Go code to temp file and run
                let temp_path = format!("/tmp/exec_{}.go", std::process::id());
                if let Err(e) = std::fs::write(&temp_path, &req.code) {
                    return Err(Status::internal(format!("Failed to write Go file: {}", e)));
                }
                ("go".to_string(), vec!["run".to_string(), temp_path.clone()], Some(temp_path))
            }
            "rust" | "rs" => {
                // Write Rust code to temp file, compile and run
                let temp_src = format!("/tmp/exec_{}.rs", std::process::id());
                let temp_bin = format!("/tmp/exec_{}", std::process::id());
                if let Err(e) = std::fs::write(&temp_src, &req.code) {
                    return Err(Status::internal(format!("Failed to write Rust file: {}", e)));
                }
                // Compile first
                let compile = std::process::Command::new("rustc")
                    .args(["-o", &temp_bin, &temp_src])
                    .output();
                let _ = std::fs::remove_file(&temp_src);
                match compile {
                    Ok(output) if output.status.success() => {
                        (temp_bin.clone(), vec![], Some(temp_bin))
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        return Err(Status::invalid_argument(format!("Rust compile error: {}", stderr)));
                    }
                    Err(e) => {
                        return Err(Status::internal(format!("Failed to run rustc: {}", e)));
                    }
                }
            }
            _ => {
                return Err(Status::invalid_argument(format!(
                    "Unsupported language: {}. Supported: python, bash/sh, javascript, go, rust",
                    req.language
                )));
            }
        };

        let timeout_duration = Duration::from_secs(if req.timeout_secs > 0 {
            req.timeout_secs as u64
        } else {
            30
        });

        // Build command with optional environment variables
        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Add environment variables if provided
        for (key, value) in &req.env {
            cmd.env(key, value);
        }

        // Execute with timeout
        let result = timeout(timeout_duration, cmd.output()).await;

        // Clean up temp files
        if let Some(ref tf) = temp_file {
            let _ = std::fs::remove_file(tf);
        }

        let duration_ms = start.elapsed().as_secs_f64() * 1000.0;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let exit_code = output.status.code().unwrap_or(-1);

                println!(
                    "[GUEST] Execution complete: exit_code={}, duration={:.2}ms, trace_id={}",
                    exit_code, duration_ms, trace_id
                );

                Ok(Response::new(ExecuteResponse {
                    stdout,
                    stderr,
                    exit_code,
                    duration_ms,
                    timed_out: false,
                }))
            }
            Ok(Err(e)) => {
                println!("[GUEST] Execution error: {}", e);
                Err(Status::internal(format!("Execution error: {}", e)))
            }
            Err(_) => {
                // Timeout occurred
                println!("[GUEST] Execution timed out after {:?}", timeout_duration);
                Ok(Response::new(ExecuteResponse {
                    stdout: String::new(),
                    stderr: "Execution timed out".to_string(),
                    exit_code: -1,
                    duration_ms,
                    timed_out: true,
                }))
            }
        }
    }

    type ExecuteStreamStream =
        tokio_stream::wrappers::ReceiverStream<Result<ExecuteChunk, Status>>;

    async fn execute_stream(
        &self,
        request: Request<ExecuteRequest>,
    ) -> Result<Response<Self::ExecuteStreamStream>, Status> {
        let req = request.into_inner();
        let start = std::time::Instant::now();

        // Extract trace_id from environment if present
        let trace_id = req.env.get("NEUROVISOR_TRACE_ID")
            .map(|s| s.as_str())
            .unwrap_or("none");

        println!(
            "[GUEST] ExecuteStream request: language={}, code_len={}, trace_id={}",
            req.language,
            req.code.len(),
            trace_id
        );

        // Build command based on language
        // Note: Alpine uses BusyBox, so we use /bin/sh for shell commands
        // For compiled languages (Go, Rust), we compile first then stream execution
        let (program, args, temp_file): (String, Vec<String>, Option<String>) = match req.language.as_str() {
            "python" | "python3" => ("python3".to_string(), vec!["-u".to_string(), "-c".to_string(), req.code.clone()], None),
            "bash" | "sh" | "shell" => ("/bin/sh".to_string(), vec!["-c".to_string(), req.code.clone()], None),
            "javascript" | "node" => ("node".to_string(), vec!["-e".to_string(), req.code.clone()], None),
            "go" | "golang" => {
                // Write Go code to temp file
                let temp_path = format!("/tmp/exec_stream_{}.go", std::process::id());
                if let Err(e) = std::fs::write(&temp_path, &req.code) {
                    return Err(Status::internal(format!("Failed to write Go file: {}", e)));
                }
                ("go".to_string(), vec!["run".to_string(), temp_path.clone()], Some(temp_path))
            }
            "rust" | "rs" => {
                // Write Rust code to temp file, compile first
                let temp_src = format!("/tmp/exec_stream_{}.rs", std::process::id());
                let temp_bin = format!("/tmp/exec_stream_{}", std::process::id());
                if let Err(e) = std::fs::write(&temp_src, &req.code) {
                    return Err(Status::internal(format!("Failed to write Rust file: {}", e)));
                }
                // Compile synchronously before streaming
                let compile = std::process::Command::new("rustc")
                    .args(["-o", &temp_bin, &temp_src])
                    .output();
                let _ = std::fs::remove_file(&temp_src);
                match compile {
                    Ok(output) if output.status.success() => {
                        (temp_bin.clone(), vec![], Some(temp_bin))
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        return Err(Status::invalid_argument(format!("Rust compile error: {}", stderr)));
                    }
                    Err(e) => {
                        return Err(Status::internal(format!("Failed to run rustc: {}", e)));
                    }
                }
            }
            _ => {
                return Err(Status::invalid_argument(format!(
                    "Unsupported language: {}. Supported: python, bash/sh, javascript, go, rust",
                    req.language
                )));
            }
        };

        let timeout_duration = Duration::from_secs(if req.timeout_secs > 0 {
            req.timeout_secs as u64
        } else {
            30
        });

        let env_vars: HashMap<String, String> = req.env.clone();

        let (tx, rx) = tokio::sync::mpsc::channel(100);

        tokio::spawn(async move {
            let mut cmd = Command::new(&program);
            cmd.args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());

            for (key, value) in &env_vars {
                cmd.env(key, value);
            }

            // Store temp_file for cleanup at the end
            let cleanup_file = temp_file;

            // Spawn the child process
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(Status::internal(format!("Failed to spawn: {}", e)))).await;
                    return;
                }
            };

            // Take stdout and stderr handles
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            // Clone tx for concurrent readers
            let tx_stdout = tx.clone();
            let tx_stderr = tx.clone();

            // Spawn stdout reader task
            let stdout_task = tokio::spawn(async move {
                if let Some(stdout) = stdout {
                    let mut reader = BufReader::new(stdout).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        let chunk = ExecuteChunk {
                            output: Some(Output::StdoutChunk(format!("{}\n", line))),
                            is_final: false,
                            metadata: None,
                        };
                        if tx_stdout.send(Ok(chunk)).await.is_err() {
                            break; // Client disconnected
                        }
                    }
                }
            });

            // Spawn stderr reader task
            let stderr_task = tokio::spawn(async move {
                if let Some(stderr) = stderr {
                    let mut reader = BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        let chunk = ExecuteChunk {
                            output: Some(Output::StderrChunk(format!("{}\n", line))),
                            is_final: false,
                            metadata: None,
                        };
                        if tx_stderr.send(Ok(chunk)).await.is_err() {
                            break; // Client disconnected
                        }
                    }
                }
            });

            // Wait for process with timeout
            let wait_result = timeout(timeout_duration, child.wait()).await;
            let duration_ms = start.elapsed().as_secs_f64() * 1000.0;

            // Wait for readers to finish
            let _ = stdout_task.await;
            let _ = stderr_task.await;

            // Send final chunk with metadata
            match wait_result {
                Ok(Ok(status)) => {
                    let _ = tx
                        .send(Ok(ExecuteChunk {
                            output: None,
                            is_final: true,
                            metadata: Some(ExecuteMetadata {
                                exit_code: status.code().unwrap_or(-1),
                                duration_ms,
                                timed_out: false,
                            }),
                        }))
                        .await;
                }
                Ok(Err(e)) => {
                    let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                }
                Err(_) => {
                    // Timeout - kill the process
                    let _ = child.kill().await;
                    let _ = tx
                        .send(Ok(ExecuteChunk {
                            output: Some(Output::StderrChunk("Execution timed out\n".to_string())),
                            is_final: true,
                            metadata: Some(ExecuteMetadata {
                                exit_code: -1,
                                duration_ms,
                                timed_out: true,
                            }),
                        }))
                        .await;
                }
            }

            // Clean up temp files
            if let Some(ref tf) = cleanup_file {
                let _ = std::fs::remove_file(tf);
            }
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            rx,
        )))
    }
}

/// Create a vsock listener using raw libc (musl-compatible)
///
/// tokio_vsock causes GPF crashes in musl-compiled binaries running in Firecracker,
/// so we use the same raw libc approach that works in vsock_test.rs.
fn create_vsock_listener() -> std::io::Result<std::os::unix::net::UnixListener> {
    // Create vsock socket
    let fd = unsafe { libc::socket(AF_VSOCK, SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Bind to vsock address
    let addr = SockaddrVm {
        svm_family: AF_VSOCK as u16,
        svm_reserved1: 0,
        svm_port: VSOCK_PORT,
        svm_cid: VMADDR_CID_ANY,
        svm_flags: 0,
        svm_zero: [0; 3],
    };

    let ret = unsafe {
        libc::bind(
            fd,
            &addr as *const SockaddrVm as *const libc::sockaddr,
            std::mem::size_of::<SockaddrVm>() as libc::socklen_t,
        )
    };

    if ret < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }

    // Listen
    let ret = unsafe { libc::listen(fd, 128) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }

    // Set non-blocking for tokio
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }

    let ret = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }

    // Wrap in UnixListener (works because vsock is SOCK_STREAM)
    Ok(unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd) })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("[GUEST] Starting execution server on vsock port {}...", VSOCK_PORT);

    let std_listener = create_vsock_listener()?;
    let listener = tokio::net::UnixListener::from_std(std_listener)?;

    let server = ExecutionServer;

    // Create incoming stream from listener
    let incoming = async_stream::stream! {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    println!("[GUEST] Accepted connection");
                    yield Ok::<_, std::io::Error>(stream);
                }
                Err(e) => {
                    eprintln!("[GUEST] Accept error: {}", e);
                    continue;
                }
            }
        }
    };

    println!("[GUEST] Execution server ready!");

    tonic::transport::Server::builder()
        .add_service(ExecutionServiceServer::new(server))
        .serve_with_incoming(incoming)
        .await?;

    Ok(())
}
