//! Host-side execution client for connecting to guest VMs via vsock
//!
//! This module provides the ExecutionClient that connects to the guest
//! execution server via the vsock Unix domain socket path.
//!
//! Firecracker vsock requires a handshake protocol for host-initiated connections:
//! 1. Connect to the main vsock UDS (e.g., ./neurovisor-{vm_id}.vsock)
//! 2. Send "CONNECT {port}\n"
//! 3. Receive "OK {host_port}\n" if guest is listening
//! 4. Use the connection for gRPC

use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tonic::transport::Endpoint;
use tower::service_fn;
use hyper_util::rt::tokio::TokioIo;

pub mod proto {
    tonic::include_proto!("neurovisor.execution");
}

pub use proto::{ExecuteRequest, ExecuteResponse, ExecuteChunk, ExecuteMetadata};
pub use proto::execute_chunk::Output as ChunkOutput;
use proto::execution_service_client::ExecutionServiceClient;

/// A chunk of streaming output from code execution
#[derive(Debug, Clone)]
pub enum OutputChunk {
    Stdout(String),
    Stderr(String),
    Done {
        exit_code: i32,
        duration_ms: f64,
        timed_out: bool,
    },
}

/// Final result after streaming execution completes
#[derive(Debug, Clone)]
pub struct StreamingResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration_ms: f64,
    pub timed_out: bool,
}

/// Error type for execution client operations
#[derive(Debug)]
pub enum ExecutionError {
    Connection(String),
    Handshake(String),
    Grpc(tonic::Status),
    Transport(tonic::transport::Error),
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecutionError::Connection(msg) => write!(f, "Connection error: {}", msg),
            ExecutionError::Handshake(msg) => write!(f, "Vsock handshake error: {}", msg),
            ExecutionError::Grpc(status) => write!(f, "gRPC error: {}", status),
            ExecutionError::Transport(e) => write!(f, "Transport error: {}", e),
        }
    }
}

impl std::error::Error for ExecutionError {}

impl From<tonic::Status> for ExecutionError {
    fn from(status: tonic::Status) -> Self {
        ExecutionError::Grpc(status)
    }
}

impl From<tonic::transport::Error> for ExecutionError {
    fn from(e: tonic::transport::Error) -> Self {
        ExecutionError::Transport(e)
    }
}

/// Perform Firecracker vsock handshake and return the connected stream
///
/// Firecracker's vsock implementation requires a text-based handshake:
/// 1. Connect to the vsock UDS
/// 2. Send "CONNECT {port}\n"
/// 3. Receive "OK {host_port}\n" or error
async fn vsock_handshake(
    vsock_path: &PathBuf,
    port: u32,
) -> Result<tokio::net::UnixStream, ExecutionError> {
    // Connect to the main vsock UDS
    let stream = tokio::net::UnixStream::connect(vsock_path)
        .await
        .map_err(|e| ExecutionError::Connection(format!("Failed to connect to {}: {}", vsock_path.display(), e)))?;

    let (reader, mut writer) = stream.into_split();

    // Send CONNECT command
    let connect_cmd = format!("CONNECT {}\n", port);
    writer.write_all(connect_cmd.as_bytes()).await
        .map_err(|e| ExecutionError::Handshake(format!("Failed to send CONNECT: {}", e)))?;

    // Read response with timeout
    let mut buf_reader = BufReader::new(reader);
    let mut response = String::new();

    let read_result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        buf_reader.read_line(&mut response)
    ).await;

    match read_result {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            return Err(ExecutionError::Handshake(format!("Failed to read response: {}", e)));
        }
        Err(_) => {
            return Err(ExecutionError::Handshake(
                "Timeout waiting for handshake response (guest may not be listening)".to_string()
            ));
        }
    }

    // Check response - should be "OK {host_port}\n"
    let response = response.trim();
    if !response.starts_with("OK ") {
        return Err(ExecutionError::Handshake(format!(
            "Unexpected handshake response: '{}' (expected 'OK <port>')",
            response
        )));
    }

    // Reunite the stream
    let stream = buf_reader.into_inner().reunite(writer)
        .map_err(|e| ExecutionError::Handshake(format!("Failed to reunite stream: {}", e)))?;

    Ok(stream)
}

/// Client for executing code in guest VMs via vsock
pub struct ExecutionClient {
    client: ExecutionServiceClient<tonic::transport::Channel>,
}

impl ExecutionClient {
    /// Connect to a guest VM's execution service via vsock
    ///
    /// # Arguments
    /// * `vsock_path` - The main vsock Unix domain socket path (e.g., ./neurovisor-{vm_id}.vsock)
    /// * `port` - The guest vsock port to connect to (e.g., 6000)
    ///
    /// # Returns
    /// An ExecutionClient connected to the guest VM
    pub async fn connect_to_port(vsock_path: PathBuf, port: u32) -> Result<Self, ExecutionError> {
        let path = vsock_path.clone();

        // Tonic requires a valid URI even for UDS connections
        // The actual connection is handled by the custom connector with handshake
        // Test handshake first to get a proper error message
        // (tonic's lazy connect hides the actual error)
        let test_stream = vsock_handshake(&path, port).await?;
        drop(test_stream); // Close the test connection

        // Now create the actual channel with tonic
        let channel = Endpoint::try_from("http://[::1]:6000")?
            .connect_with_connector(service_fn(move |_| {
                let p = path.clone();
                async move {
                    let stream = vsock_handshake(&p, port).await
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
                    Ok::<_, std::io::Error>(TokioIo::new(stream))
                }
            }))
            .await
            .map_err(|e| ExecutionError::Connection(format!("gRPC channel error: {}", e)))?;

        Ok(Self {
            client: ExecutionServiceClient::new(channel),
        })
    }

    /// Connect to a guest VM's execution service via vsock UDS path (legacy)
    ///
    /// This method expects a path like `./neurovisor-{vm_id}.vsock_6000` but actually
    /// uses the base vsock path with handshake. Kept for backward compatibility.
    ///
    /// # Arguments
    /// * `vsock_path` - The vsock path with port suffix (e.g., ./neurovisor-{vm_id}.vsock_6000)
    ///
    /// # Returns
    /// An ExecutionClient connected to the guest VM
    pub async fn connect(vsock_path: PathBuf) -> Result<Self, ExecutionError> {
        // Parse port from path suffix (e.g., "./foo.vsock_6000" -> port=6000, base="./foo.vsock")
        let path_str = vsock_path.to_string_lossy();
        let (base_path, port) = if let Some(idx) = path_str.rfind('_') {
            let port_str = &path_str[idx + 1..];
            if let Ok(port) = port_str.parse::<u32>() {
                (PathBuf::from(&path_str[..idx]), port)
            } else {
                return Err(ExecutionError::Connection(format!(
                    "Invalid vsock path format: {}. Expected {{base}}_{{port}}",
                    vsock_path.display()
                )));
            }
        } else {
            return Err(ExecutionError::Connection(format!(
                "Invalid vsock path format: {}. Expected {{base}}_{{port}}",
                vsock_path.display()
            )));
        };

        Self::connect_to_port(base_path, port).await
    }

    /// Connect with retry logic for when guest may not be ready
    ///
    /// # Arguments
    /// * `vsock_path` - The vsock Unix domain socket path (with port suffix like `foo.vsock_6000`)
    /// * `max_retries` - Maximum number of connection attempts
    /// * `retry_delay_ms` - Delay between retries in milliseconds
    pub async fn connect_with_retry(
        vsock_path: PathBuf,
        max_retries: u32,
        retry_delay_ms: u64,
    ) -> Result<Self, ExecutionError> {
        // Parse the base path (without port suffix) for existence checks
        let path_str = vsock_path.to_string_lossy();
        let base_path = if let Some(idx) = path_str.rfind('_') {
            PathBuf::from(&path_str[..idx])
        } else {
            vsock_path.clone()
        };

        let mut last_error = None;

        for attempt in 0..max_retries {
            match Self::connect(vsock_path.clone()).await {
                Ok(client) => {
                    return Ok(client);
                }
                Err(e) => {
                    if attempt == 0 {
                        let exists = base_path.exists();
                        println!("[EXEC] Connection attempt failed: {} (socket exists: {})", e, exists);
                    }
                    last_error = Some(e);
                    if attempt + 1 < max_retries {
                        tokio::time::sleep(tokio::time::Duration::from_millis(retry_delay_ms)).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            ExecutionError::Connection("Max retries reached".to_string())
        }))
    }

    /// Execute code in the guest VM
    ///
    /// # Arguments
    /// * `language` - The programming language ("python", "bash", "javascript")
    /// * `code` - The code to execute
    /// * `timeout_secs` - Maximum execution time in seconds
    ///
    /// # Returns
    /// ExecuteResponse containing stdout, stderr, exit_code, and timing info
    pub async fn execute(
        &mut self,
        language: &str,
        code: &str,
        timeout_secs: u32,
    ) -> Result<ExecuteResponse, ExecutionError> {
        let request = ExecuteRequest {
            language: language.to_string(),
            code: code.to_string(),
            timeout_secs,
            env: std::collections::HashMap::new(),
        };

        let response = self.client.execute(request).await?;
        Ok(response.into_inner())
    }

    /// Execute code with environment variables
    pub async fn execute_with_env(
        &mut self,
        language: &str,
        code: &str,
        timeout_secs: u32,
        env: std::collections::HashMap<String, String>,
    ) -> Result<ExecuteResponse, ExecutionError> {
        let request = ExecuteRequest {
            language: language.to_string(),
            code: code.to_string(),
            timeout_secs,
            env,
        };

        let response = self.client.execute(request).await?;
        Ok(response.into_inner())
    }

    /// Execute code with streaming output
    ///
    /// Calls the provided callback for each output chunk (stdout/stderr) as it arrives.
    /// Returns the final aggregated result when execution completes.
    pub async fn execute_streaming<F>(
        &mut self,
        language: &str,
        code: &str,
        timeout_secs: u32,
        mut on_output: F,
    ) -> Result<StreamingResult, ExecutionError>
    where
        F: FnMut(OutputChunk),
    {
        let request = ExecuteRequest {
            language: language.to_string(),
            code: code.to_string(),
            timeout_secs,
            env: std::collections::HashMap::new(),
        };

        let response = self.client.execute_stream(request).await?;
        let mut stream = response.into_inner();

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = 0;
        let mut duration_ms = 0.0;
        let mut timed_out = false;

        use tokio_stream::StreamExt;

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result?;

            if let Some(output) = chunk.output {
                match output {
                    ChunkOutput::StdoutChunk(s) => {
                        on_output(OutputChunk::Stdout(s.clone()));
                        stdout.push_str(&s);
                    }
                    ChunkOutput::StderrChunk(s) => {
                        on_output(OutputChunk::Stderr(s.clone()));
                        stderr.push_str(&s);
                    }
                }
            }

            if chunk.is_final {
                if let Some(meta) = chunk.metadata {
                    exit_code = meta.exit_code;
                    duration_ms = meta.duration_ms;
                    timed_out = meta.timed_out;
                }
                on_output(OutputChunk::Done {
                    exit_code,
                    duration_ms,
                    timed_out,
                });
                break;
            }
        }

        Ok(StreamingResult {
            stdout,
            stderr,
            exit_code,
            duration_ms,
            timed_out,
        })
    }
}
