//! Firecracker VM management module
//!
//! This module provides abstractions for managing Firecracker VMs including:
//! - VM configuration (boot source, drives, vsock)
//! - Firecracker API client
//! - VM lifecycle management (spawn, snapshot, restore)
//! - VM pooling for multi-VM orchestration

pub mod config;
pub mod firecracker;
pub mod handle;
pub mod lifecycle;
pub mod manager;
pub mod pool;

pub use config::*;
pub use firecracker::FirecrackerClient;
pub use handle::{VMHandle, VMStatus};
pub use lifecycle::*;
pub use manager::{VMManager, VMManagerConfig};
pub use pool::{VMPool, PoolStats};
