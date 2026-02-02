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

pub mod agent;
pub mod cgroups;
pub mod grpc;
pub mod metrics;
pub mod ollama;
pub mod security;
pub mod vm;
