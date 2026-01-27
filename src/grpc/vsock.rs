//! Custom vsock stream wrapper that implements tonic's `Connected` trait.
//!
//! This module bridges tokio_vsock with tonic's server requirements.

use tokio_vsock::VsockStream;
use tokio::io::{AsyncRead, AsyncWrite};
use std::pin::Pin;
use std::task::{Context, Poll};

/// Wrapper around VsockStream that implements tonic's `Connected` trait
///
/// Vsock is a virtio-based communication mechanism between host and guest VMs.
/// This wrapper allows tonic to accept vsock connections by implementing the
/// `Connected` trait, which tonic uses to extract connection metadata.
///
/// Since vsock doesn't have TCP-style addresses, we use unit type `()` for
/// the ConnectInfo, similar to tokio::io::DuplexStream.
pub struct VsockConnectedStream(pub VsockStream);

/// Delegate AsyncRead to the inner VsockStream
impl AsyncRead for VsockConnectedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

/// Delegate AsyncWrite to the inner VsockStream
impl AsyncWrite for VsockConnectedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// Implement tonic's Connected trait for vsock streams
///
/// The Connected trait is required by tonic's Server::serve_with_incoming() to
/// extract metadata about each connection. For vsock, we don't have TCP addresses,
/// so ConnectInfo is unit type `()`.
///
/// Key point: this enables vsock streams to be used with tonic's server builder
impl tonic::transport::server::Connected for VsockConnectedStream {
    type ConnectInfo = ();

    fn connect_info(&self) -> Self::ConnectInfo {
        // Vsock doesn't have TCP-style peer/local addresses
        // Return unit type to satisfy the trait
    }
}
