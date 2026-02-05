//! Agent module for LLM-driven code execution
//!
//! This module provides the agent loop that orchestrates:
//! - Ollama LLM with tool calling (/api/chat)
//! - VM pool for sandboxed execution
//! - Code execution via vsock gRPC
//!
//! # Architecture
//!
//! ```text
//! User Task → AgentController → Ollama /api/chat (with tools)
//!                  ↓
//!           Tool Call: execute_code
//!                  ↓
//!           VMPool.acquire() → VM
//!                  ↓
//!           ExecutionClient (vsock) → Guest ExecutionServer
//!                  ↓
//!           Code runs in VM → stdout/stderr/exit_code
//!                  ↓
//!           VMPool.release() (destroy VM)
//!                  ↓
//!           Feed result back to Ollama → Loop or Complete
//! ```

pub mod controller;
pub mod sessions;

pub use controller::{AgentConfig, AgentController, AgentError, AgentResult, ExecutionRecord};
pub use sessions::{Session, SessionStore, SessionSummary};
