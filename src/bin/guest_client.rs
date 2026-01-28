//! Guest client that connects to the host via vsock and sends a gRPC inference request.
//!
//! Uses raw libc vsock calls instead of tokio_vsock to avoid a GPF crash
//! in musl-compiled binaries running inside Firecracker VMs.

use std::collections::HashMap;
use std::env;
use std::os::unix::io::FromRawFd;

use tonic::transport::Endpoint;
use tower::service_fn;
use hyper_util::rt::tokio::TokioIo;
use neurovisor::grpc::inference::inference_service_client::InferenceServiceClient;
use neurovisor::grpc::inference::InferenceRequest;

const AF_VSOCK: libc::c_int = 40;
const SOCK_STREAM: libc::c_int = 1;
const VMADDR_CID_HOST: u32 = 2;
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

/// Create a raw vsock socket, connect to host, and return as a tokio UnixStream.
///
/// This bypasses tokio_vsock entirely, using the same raw libc approach
/// that vsock_test.rs uses (which works in Firecracker).
fn connect_vsock_raw() -> std::io::Result<std::os::unix::net::UnixStream> {
    let fd = unsafe { libc::socket(AF_VSOCK, SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let addr = SockaddrVm {
        svm_family: AF_VSOCK as u16,
        svm_reserved1: 0,
        svm_port: VSOCK_PORT,
        svm_cid: VMADDR_CID_HOST,
        svm_flags: 0,
        svm_zero: [0; 3],
    };

    let ret = unsafe {
        libc::connect(
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

    // Set non-blocking for tokio compatibility
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

    // Wrap the raw fd as a UnixStream - this works because both are SOCK_STREAM fds
    // and tokio only cares about the fd for async I/O polling
    Ok(unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let prompt = env::args().nth(1).unwrap_or_else(|| "Hello what is AI?".to_string());

    println!("[GUEST] Connecting to host via vsock (raw libc)...");
    let channel = Endpoint::try_from("http://[::1]:6000")?
        .connect_with_connector(service_fn(|_| async {
            println!("[GUEST] Opening vsock connection to CID {}, port {}...", VMADDR_CID_HOST, VSOCK_PORT);
            let std_stream = connect_vsock_raw()?;
            let tokio_stream = tokio::net::UnixStream::from_std(std_stream)?;
            Ok::<_, std::io::Error>(TokioIo::new(tokio_stream))
        }))
        .await?;

    println!("[GUEST] Connected to host!");

    let mut client = InferenceServiceClient::new(channel);

    let request = InferenceRequest {
        prompt,
        model: String::new(),
        temperature: 0.7,
        max_tokens: 512,
        stream: false,
        metadata: HashMap::new(),
    };

    println!("[GUEST] Sending inference request...");
    let response = client.infer(request).await?;
    let infer_response = response.into_inner();

    println!("[GUEST] Response: {}", infer_response.response);
    println!("[GUEST] Tokens: {}", infer_response.tokens_generated);
    println!("[GUEST] Model: {}", infer_response.model_used);

    Ok(())
}