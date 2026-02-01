//! Ollama API client for LLM inference

use futures_util::stream::{Stream, StreamExt};
use std::pin::Pin;

/// Represents a single item from the streaming response.
///
/// In Rust, enums can hold different types of data in each variant.
/// This is called a "tagged union" - the enum "tag" tells you which
/// variant it is, and each variant can contain different data.
///
/// This lets us return EITHER a token string OR final metadata from
/// the same stream, and the caller can pattern match to handle each case.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    /// A token fragment from the model's response
    Token(String),
    /// The final message containing timing/count metadata
    Done(GenerateResponse),
}

/// Response from Ollama's generate endpoint with metadata
#[derive(Debug, Clone)]
pub struct GenerateResponse {
    /// The generated text
    pub response: String,
    /// Number of tokens generated
    pub eval_count: u32,
    /// Number of tokens in the prompt
    pub prompt_eval_count: u32,
    /// Time spent generating tokens (nanoseconds)
    pub eval_duration_ns: u64,
}

/// Client for interacting with Ollama's HTTP API
#[derive(Clone)]
pub struct OllamaClient {
    base_url: String,
    client: reqwest::Client,
}

impl OllamaClient {
    /// Create a new Ollama client
    ///
    /// # Arguments
    /// * `base_url` - The base URL of the Ollama server (e.g., "http://localhost:11434")
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Generate a streaming response from Ollama
    ///
    /// # Arguments
    /// * `prompt` - The input prompt for the LLM
    /// * `model` - The model name (e.g., "llama3.2")
    /// * `trace_id` - Optional trace ID for request correlation
    ///
    /// # Returns
    /// A stream of `StreamChunk` items - either `Token(String)` for each
    /// generated token, or `Done(GenerateResponse)` for the final metadata.
    ///
    /// # How Rust Streams Work
    /// A `Stream` is like an async iterator - it yields items one at a time.
    /// `Pin<Box<dyn Stream<...> + Send>>` means:
    /// - `Pin<Box<...>>` - heap-allocated and pinned in memory (required for async)
    /// - `dyn Stream<...>` - a trait object (any type implementing Stream)
    /// - `+ Send` - can be sent across threads safely
    pub async fn generate_stream(
        &self,
        prompt: impl Into<String>,
        model: impl Into<String>,
        trace_id: Option<&str>,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<StreamChunk, Box<dyn std::error::Error + Send + Sync>>> + Send>>,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        let endpoint = format!("{}/api/generate", self.base_url);
        let prompt = prompt.into();
        let model = model.into();

        // Build request with optional trace ID header
        let mut request = self
            .client
            .post(&endpoint)
            .json(&serde_json::json!({
                "model": model,
                "prompt": prompt,
                "stream": true
            }));

        if let Some(tid) = trace_id {
            request = request.header("X-Trace-Id", tid);
        }

        let bytes_stream = request.send().await?.bytes_stream();

        // Transform the byte stream into a StreamChunk stream
        //
        // .map() transforms each item in the stream. For each chunk of bytes
        // from Ollama, we parse the JSON and decide what StreamChunk to return.
        let token_stream = bytes_stream.map(|chunk_result| {
            match chunk_result {
                Ok(bytes) => {
                    // Parse the JSON chunk from Ollama
                    match serde_json::from_slice::<serde_json::Value>(&bytes) {
                        Ok(data) => {
                            // Check if this is the final chunk (done: true)
                            // Ollama sends metadata only in the final message
                            if data["done"].as_bool() == Some(true) {
                                // Extract all the metadata from the final chunk
                                let response = GenerateResponse {
                                    response: data["response"].as_str().unwrap_or("").to_string(),
                                    eval_count: data["eval_count"].as_u64().unwrap_or(0) as u32,
                                    prompt_eval_count: data["prompt_eval_count"].as_u64().unwrap_or(0) as u32,
                                    eval_duration_ns: data["eval_duration"].as_u64().unwrap_or(0),
                                };
                                Ok(StreamChunk::Done(response))
                            } else if let Some(token) = data["response"].as_str() {
                                // Regular token - wrap it in StreamChunk::Token
                                Ok(StreamChunk::Token(token.to_string()))
                            } else {
                                // Chunk without a token (shouldn't happen often)
                                Ok(StreamChunk::Token(String::new()))
                            }
                        }
                        Err(e) => Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>),
                    }
                }
                Err(e) => Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>),
            }
        });

        Ok(Box::pin(token_stream))
    }

    /// Generate a complete (non-streaming) response from Ollama
    ///
    /// # Arguments
    /// * `prompt` - The input prompt for the LLM
    /// * `model` - The model name (e.g., "llama3.2")
    /// * `trace_id` - Optional trace ID for request correlation
    ///
    /// # Returns
    /// GenerateResponse with the text and token/timing metadata
    pub async fn generate(
        &self,
        prompt: impl Into<String>,
        model: impl Into<String>,
        trace_id: Option<&str>,
    ) -> Result<GenerateResponse, Box<dyn std::error::Error + Send + Sync>> {
        let endpoint = format!("{}/api/generate", self.base_url);
        let prompt = prompt.into();
        let model = model.into();

        // Build request with optional trace ID header
        let mut request = self
            .client
            .post(&endpoint)
            .json(&serde_json::json!({
                "model": model,
                "prompt": prompt,
                "stream": true
            }));

        if let Some(tid) = trace_id {
            request = request.header("X-Trace-Id", tid);
        }

        let mut bytes_stream = request.send().await?.bytes_stream();

        let mut full_response = String::new();
        let mut eval_count = 0u32;
        let mut prompt_eval_count = 0u32;
        let mut eval_duration_ns = 0u64;

        while let Some(chunk_result) = bytes_stream.next().await {
            let bytes = chunk_result?;
            if let Ok(data) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                // Accumulate response tokens
                if let Some(token) = data["response"].as_str() {
                    full_response.push_str(token);
                }

                // Check if this is the final chunk with metadata
                if data["done"].as_bool() == Some(true) {
                    eval_count = data["eval_count"].as_u64().unwrap_or(0) as u32;
                    prompt_eval_count = data["prompt_eval_count"].as_u64().unwrap_or(0) as u32;
                    eval_duration_ns = data["eval_duration"].as_u64().unwrap_or(0);
                }
            }
        }

        Ok(GenerateResponse {
            response: full_response,
            eval_count,
            prompt_eval_count,
            eval_duration_ns,
        })
    }
}
