//! NeuroVisor - Firecracker-based VM manager with Ollama LLM integration
//!
//! This library provides a modular architecture for managing Firecracker VMs
//! and coordinating AI inference requests between guests and host via gRPC.

pub mod vm;
pub mod ollama;
pub mod grpc;
