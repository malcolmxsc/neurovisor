//! Ollama LLM integration module
//!
//! This module provides clients for interacting with Ollama's API:
//! - `OllamaClient` - Basic generate endpoint for streaming inference
//! - `ChatClient` - Chat endpoint with tool/function calling support

pub mod client;
pub mod tool_use;

// Re-export public types from the client module
pub use client::{GenerateResponse, OllamaClient, StreamChunk};

// Re-export chat/tool types for agent workflows
pub use tool_use::{
    parse_tool_calls_from_text, ChatClient, ChatError, ChatMessage, ChatResponse, FunctionCall,
    Tool, ToolCall, ToolFunction, DEFAULT_AGENT_SYSTEM_PROMPT,
};
