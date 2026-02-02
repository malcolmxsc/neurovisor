//! gRPC server module for host-guest communication
//!
//! This module provides:
//! - InferenceService gRPC server implementation (single VM mode)
//! - GatewayServer for multi-VM pool orchestration
//! - Protobuf message handling

pub mod server;
pub mod gateway;

pub use server::inference;
pub use server::InferenceServer;
pub use gateway::GatewayServer;
