//! gRPC server module for host-guest communication
//!
//! This module provides:
//! - InferenceService gRPC server implementation (single VM mode)
//! - GatewayServer for multi-VM pool orchestration
//! - ExecutionClient for code execution in guest VMs
//! - Protobuf message handling

pub mod execution;
pub mod gateway;
pub mod server;

pub use execution::{ExecutionClient, ExecutionError, OutputChunk, StreamingResult};
pub use gateway::GatewayServer;
pub use server::inference;
pub use server::InferenceServer;
