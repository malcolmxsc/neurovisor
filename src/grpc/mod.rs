//! gRPC server module for host-guest communication
//!
//! This module provides:
//! - InferenceService gRPC server implementation
//! - Custom vsock transport for tonic
//! - Protobuf message handling

pub mod server;
pub mod vsock;
pub use server::inference;
pub use vsock::VsockConnectedStream;
