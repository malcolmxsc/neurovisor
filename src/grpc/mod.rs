//! gRPC server module for host-guest communication
//!
//! This module provides:
//! - InferenceService gRPC server implementation
//! - Protobuf message handling

pub mod server;
pub use server::inference;
