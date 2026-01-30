//! Ollama LLM integration module
//!
//! This module provides a client for interacting with Ollama's API,
//! including streaming token generation for inference requests.

pub mod client;

// Re-export public types from the client module
// This lets other modules do `use crate::ollama::StreamChunk` instead of
// `use crate::ollama::client::StreamChunk`
pub use client::{OllamaClient, GenerateResponse, StreamChunk};
