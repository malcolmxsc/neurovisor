//! NeuroVisor - Firecracker-based VM manager with Ollama LLM integration
//!
//! This library provides a modular architecture for managing Firecracker VMs
//! and coordinating AI inference requests between guests and host via gRPC.
//!
//! # Modules
//!
//! - `vm` - Firecracker VM lifecycle management
//! - `ollama` - Ollama LLM client for inference
//! - `grpc` - gRPC server for host-guest communication
//! - `cgroups` - Resource isolation using Linux cgroups v2
//! - `metrics` - Prometheus metrics for observability
//! - `security` - Seccomp filters and capability dropping
//! - `ebpf` - eBPF-based syscall tracing (optional, requires `--features ebpf`)
//!
//! # Quick Start
//!
//! ```ignore
//! use neurovisor::{VMSize, VMPool};
//!
//! // Create a VM pool with medium-sized VMs
//! let pool = VMPool::new(VMSize::Medium.limits()).await?;
//!
//! // Acquire a VM for code execution (with optional trace_id for distributed tracing)
//! let vm = pool.acquire(None).await?;
//! ```

pub mod agent;
pub mod cgroups;
pub mod ebpf;
pub mod grpc;
pub mod metrics;
pub mod ollama;
pub mod security;
pub mod tracing;
pub mod vm;

// Re-export commonly used types at crate root for convenience
pub use cgroups::{ResourceLimits, VMSize};
pub use vm::{VMHandle, VMManager, VMPool};
