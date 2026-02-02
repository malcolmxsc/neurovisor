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

// Include the generated proto code for execution service
pub mod proto {
    tonic::include_proto!("neurovisor.execution");
}

pub use proto::{ExecuteRequest, ExecuteResponse, ExecuteChunk, ExecuteMetadata};
use proto::execution_service_client::ExecutionServiceClient;

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

    // Read response
    let mut buf_reader = BufReader::new(reader);
    let mut response = String::new();
    buf_reader.read_line(&mut response).await
        .map_err(|e| ExecutionError::Handshake(format!("Failed to read response: {}", e)))?;

    // Check response - should be "OK {host_port}\n"
    let response = response.trim();
    if !response.starts_with("OK ") {
        return Err(ExecutionError::Handshake(format!(
            "Unexpected response: '{}' (expected 'OK <port>')",
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
            .map_err(|e| ExecutionError::Connection(format!("Failed to connect to {}: {}", vsock_path.display(), e)))?;

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
                    last_error = Some(e);
                    if attempt + 1 < max_retries {
                        // Only log on first failure
                        if attempt == 0 {
                            let exists = base_path.exists();
                            if !exists {
                                println!("[EXEC] Waiting for guest (socket not ready)...");
                            }
                        }
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
}
