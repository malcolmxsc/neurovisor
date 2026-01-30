//! Ollama LLM integration module
//!
//! This module provides a client for interacting with Ollama's API,
//! including streaming token generation for inference requests.

pub mod client;

pub use client::{OllamaClient, GenerateResponse};
