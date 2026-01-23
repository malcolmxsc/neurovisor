//! Firecracker API client
//!
//! Provides a high-level client for interacting with Firecracker's HTTP API
//! over Unix domain sockets.

use hyper_util::client::legacy::Client;
use hyperlocal::UnixConnector;
use http_body_util::Full;
use hyper::body::Bytes;
use serde::Serialize;

use super::config::*;

type HyperClient = Client<UnixConnector, Full<Bytes>>;

/// Client for communicating with Firecracker's control API
pub struct FirecrackerClient {
    client: HyperClient,
    socket_path: String,
}

impl FirecrackerClient {
    /// Create a new Firecracker API client
    ///
    /// # Arguments
    /// * `socket_path` - Path to the Firecracker API Unix socket
    pub fn new(socket_path: impl Into<String>) -> Self {
        let client = Client::builder(hyper_util::rt::TokioExecutor::new())
            .build(UnixConnector);

        Self {
            client,
            socket_path: socket_path.into(),
        }
    }

    /// Send a generic HTTP request to the Firecracker API
    async fn send_request<T: Serialize>(
        &self,
        method: &str,
        endpoint: &str,
        body: T,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let uri: hyper::Uri = hyperlocal::Uri::new(&self.socket_path, endpoint).into();
        let json = serde_json::to_string(&body)?;

        let req_method = match method {
            "PUT" => hyper::Method::PUT,
            "PATCH" => hyper::Method::PATCH,
            _ => hyper::Method::GET,
        };

        let req = hyper::Request::builder()
            .method(req_method)
            .uri(uri)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(json)))?;

        let res = self.client.request(req).await?;
        let status = res.status();

        if !status.is_success() {
            let body_bytes = http_body_util::BodyExt::collect(res.into_body())
                .await?
                .to_bytes();
            let error_msg = String::from_utf8(body_bytes.to_vec())?;
            panic!(
                "\r\n❌ API ERROR on {}: {} - {}\r\n",
                endpoint, status, error_msg
            );
        }

        Ok(())
    }

    /// Configure the boot source (kernel and boot arguments)
    pub async fn boot_source(
        &self,
        kernel_image_path: impl Into<String>,
        boot_args: impl Into<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.send_request(
            "PUT",
            "/boot-source",
            BootSource {
                kernel_image_path: kernel_image_path.into(),
                boot_args: boot_args.into(),
            },
        )
        .await
    }

    /// Add a block device (drive) to the VM
    pub async fn add_drive(
        &self,
        drive_id: impl Into<String>,
        path_on_host: impl Into<String>,
        is_root_device: bool,
        is_read_only: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let drive_id_str = drive_id.into();
        let endpoint = format!("/drives/{}", drive_id_str);

        self.send_request(
            "PUT",
            &endpoint,
            Drive {
                drive_id: drive_id_str,
                path_on_host: path_on_host.into(),
                is_root_device,
                is_read_only,
            },
        )
        .await
    }

    /// Configure the vsock device for host-guest communication
    pub async fn configure_vsock(
        &self,
        guest_cid: u32,
        uds_path: impl Into<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.send_request(
            "PUT",
            "/vsock",
            Vsock {
                guest_cid,
                uds_path: uds_path.into(),
            },
        )
        .await
    }

    /// Start the VM instance
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.send_request(
            "PUT",
            "/actions",
            Action {
                action_type: "InstanceStart".to_string(),
            },
        )
        .await
    }

    /// Pause the VM
    pub async fn pause(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.send_request(
            "PATCH",
            "/vm",
            VmState {
                state: "Paused".to_string(),
            },
        )
        .await
    }

    /// Create a full snapshot of the VM
    pub async fn create_snapshot(
        &self,
        snapshot_path: impl Into<String>,
        mem_file_path: impl Into<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.send_request(
            "PUT",
            "/snapshot/create",
            SnapshotConfig {
                snapshot_type: "Full".to_string(),
                snapshot_path: snapshot_path.into(),
                mem_file_path: mem_file_path.into(),
            },
        )
        .await
    }

    /// Load a snapshot and optionally resume the VM
    pub async fn load_snapshot(
        &self,
        snapshot_path: impl Into<String>,
        mem_file_path: impl Into<String>,
        resume_vm: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.send_request(
            "PUT",
            "/snapshot/load",
            SnapshotLoad {
                snapshot_path: snapshot_path.into(),
                mem_file_path: mem_file_path.into(),
                resume_vm,
            },
        )
        .await
    }
}
