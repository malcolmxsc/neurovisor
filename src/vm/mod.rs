//! Firecracker VM management module
//!
//! This module provides abstractions for managing Firecracker VMs including:
//! - VM configuration (boot source, drives, vsock)
//! - Firecracker API client
//! - VM lifecycle management (spawn, snapshot, restore)

pub mod config;
pub mod firecracker;
pub mod lifecycle;

pub use config::*;
pub use firecracker::FirecrackerClient;
pub use lifecycle::*;
