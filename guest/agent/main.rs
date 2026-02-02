//! Guest execution server - runs inside Firecracker VM
//!
//! Listens on vsock for code execution requests from the host.
//! Uses raw libc vsock calls for musl compatibility (tokio_vsock causes GPF in musl).

use std::collections::HashMap;
use std::os::unix::io::FromRawFd;
use std::process::Stdio;

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

        println!(
            "[GUEST] Execute request: language={}, code_len={}",
            req.language,
            req.code.len()
        );

        // Build command based on language
        // Note: Alpine uses BusyBox, so we use /bin/sh for shell commands
        let (program, args): (&str, Vec<&str>) = match req.language.as_str() {
            "python" | "python3" => ("python3", vec!["-c", &req.code]),
            "bash" | "sh" | "shell" => ("/bin/sh", vec!["-c", &req.code]),
            "javascript" | "node" => ("node", vec!["-e", &req.code]),
            _ => {
                return Err(Status::invalid_argument(format!(
                    "Unsupported language: {}. Supported: python, bash/sh, javascript",
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
        let mut cmd = Command::new(program);
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

        let duration_ms = start.elapsed().as_secs_f64() * 1000.0;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let exit_code = output.status.code().unwrap_or(-1);

                println!(
                    "[GUEST] Execution complete: exit_code={}, duration={:.2}ms",
                    exit_code, duration_ms
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

        println!(
            "[GUEST] ExecuteStream request: language={}, code_len={}",
            req.language,
            req.code.len()
        );

        // Build command based on language
        // Note: Alpine uses BusyBox, so we use /bin/sh for shell commands
        let (program, args): (&str, Vec<String>) = match req.language.as_str() {
            "python" | "python3" => ("python3", vec!["-u".to_string(), "-c".to_string(), req.code.clone()]),
            "bash" | "sh" | "shell" => ("/bin/sh", vec!["-c".to_string(), req.code.clone()]),
            "javascript" | "node" => ("node", vec!["-e".to_string(), req.code.clone()]),
            _ => {
                return Err(Status::invalid_argument(format!(
                    "Unsupported language: {}. Supported: python, bash/sh, javascript",
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
            let mut cmd = Command::new(program);
            cmd.args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());

            for (key, value) in &env_vars {
                cmd.env(key, value);
            }

            let result = timeout(timeout_duration, cmd.output()).await;
            let duration_ms = start.elapsed().as_secs_f64() * 1000.0;

            match result {
                Ok(Ok(output)) => {
                    // Send stdout chunks
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    if !stdout.is_empty() {
                        let _ = tx
                            .send(Ok(ExecuteChunk {
                                output: Some(Output::StdoutChunk(stdout)),
                                is_final: false,
                                metadata: None,
                            }))
                            .await;
                    }

                    // Send stderr chunks
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    if !stderr.is_empty() {
                        let _ = tx
                            .send(Ok(ExecuteChunk {
                                output: Some(Output::StderrChunk(stderr)),
                                is_final: false,
                                metadata: None,
                            }))
                            .await;
                    }

                    // Send final chunk with metadata
                    let _ = tx
                        .send(Ok(ExecuteChunk {
                            output: None,
                            is_final: true,
                            metadata: Some(ExecuteMetadata {
                                exit_code: output.status.code().unwrap_or(-1),
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
                    // Timeout
                    let _ = tx
                        .send(Ok(ExecuteChunk {
                            output: Some(Output::StderrChunk("Execution timed out".to_string())),
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
