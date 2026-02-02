//! Ollama Chat API with tool calling support
//!
//! This module provides a client for Ollama's `/api/chat` endpoint,
//! which supports tool/function calling for agentic workflows.

use serde::{Deserialize, Serialize};

/// A message in a chat conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String, // "system", "user", "assistant", "tool"
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
            tool_calls: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
            tool_calls: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
            tool_calls: None,
        }
    }

    pub fn tool(content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: content.into(),
            tool_calls: None,
        }
    }
}

/// A tool call from the model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub function: FunctionCall,
}

/// Function call details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Tool definition for the model
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub tool_type: String, // Always "function"
    pub function: ToolFunction,
}

/// Function specification for a tool
#[derive(Debug, Clone, Serialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value, // JSON Schema
}

/// Response from /api/chat
#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    pub message: ChatMessage,
    pub done: bool,
    #[serde(default)]
    pub eval_count: u32,
    #[serde(default)]
    pub eval_duration: u64,
}

/// Error type for chat operations
#[derive(Debug)]
pub enum ChatError {
    Request(reqwest::Error),
    Parse(serde_json::Error),
    EmptyResponse,
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatError::Request(e) => write!(f, "Request error: {}", e),
            ChatError::Parse(e) => write!(f, "Parse error: {}", e),
            ChatError::EmptyResponse => write!(f, "Empty response from Ollama"),
        }
    }
}

impl std::error::Error for ChatError {}

impl From<reqwest::Error> for ChatError {
    fn from(e: reqwest::Error) -> Self {
        ChatError::Request(e)
    }
}

impl From<serde_json::Error> for ChatError {
    fn from(e: serde_json::Error) -> Self {
        ChatError::Parse(e)
    }
}

/// Client for Ollama's /api/chat endpoint with tool support
#[derive(Clone)]
pub struct ChatClient {
    base_url: String,
    client: reqwest::Client,
}

impl ChatClient {
    /// Create a new chat client
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Send a chat request with optional tools
    ///
    /// # Arguments
    /// * `messages` - The conversation history
    /// * `model` - The model name (e.g., "llama3.2")
    /// * `tools` - Optional list of tools the model can use
    ///
    /// # Returns
    /// ChatResponse containing the model's reply and any tool calls
    pub async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        model: &str,
        tools: Option<Vec<Tool>>,
    ) -> Result<ChatResponse, ChatError> {
        let endpoint = format!("{}/api/chat", self.base_url);

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
            "options": {
                "temperature": 0.0
            }
        });

        if let Some(t) = tools {
            body["tools"] = serde_json::to_value(t)?;
        }

        let response = self.client.post(&endpoint).json(&body).send().await?;

        let text = response.text().await?;

        if text.is_empty() {
            return Err(ChatError::EmptyResponse);
        }

        let chat_response: ChatResponse = serde_json::from_str(&text)?;
        Ok(chat_response)
    }

    /// Create the execute_code tool definition
    ///
    /// This tool allows the model to execute code in a sandboxed environment.
    pub fn execute_code_tool() -> Tool {
        Tool {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "execute_code".to_string(),
                description: "Execute code in a sandboxed environment. Use this to run Python or Bash code and see the output. The environment is isolated and secure.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "language": {
                            "type": "string",
                            "enum": ["python", "bash", "javascript"],
                            "description": "The programming language to use"
                        },
                        "code": {
                            "type": "string",
                            "description": "The code to execute"
                        }
                    },
                    "required": ["language", "code"]
                }),
            },
        }
    }
}

/// Default system prompt for code execution agent
pub const DEFAULT_AGENT_SYSTEM_PROMPT: &str = r#"You are a helpful assistant with access to a sandboxed code execution environment.

You have access to the execute_code tool which runs code in an isolated VM. Use it whenever you need to:
- Calculate something
- Verify a result
- Run shell commands
- Test code

Guidelines:
- Always use the execute_code tool to verify results rather than guessing
- If code fails, read the error message and fix it
- Supported languages: bash, python, javascript
- When the task is complete, respond with your final answer in plain text"#;

/// Try to parse tool calls from the response content text
///
/// This handles models that output tool calls as JSON in the text
/// instead of using the native tool_calls field.
pub fn parse_tool_calls_from_text(content: &str) -> Vec<ToolCall> {
    let mut tool_calls = Vec::new();

    // Try to find JSON objects in the content
    let content = content.trim();

    // Try parsing the entire content as a tool call
    if let Some(tool_call) = try_parse_tool_call(content) {
        tool_calls.push(tool_call);
        return tool_calls;
    }

    // Try to find JSON objects within the text
    // Look for patterns like {...} that might be tool calls
    let mut depth = 0;
    let mut start = None;

    for (i, c) in content.char_indices() {
        match c {
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        let json_str = &content[s..=i];
                        if let Some(tool_call) = try_parse_tool_call(json_str) {
                            tool_calls.push(tool_call);
                        }
                    }
                    start = None;
                }
            }
            _ => {}
        }
    }

    tool_calls
}

/// Try to parse a single tool call from a JSON string
fn try_parse_tool_call(json_str: &str) -> Option<ToolCall> {
    // First, try standard JSON parsing
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) {
        return parse_tool_call_from_value(&value);
    }

    // Fallback: try to fix common JSON errors (unescaped quotes in code field)
    // Pattern: {"name": "execute_code", "arguments": {"language": "bash", "code": "echo "hello""}}
    if let Some(fixed) = try_fix_malformed_json(json_str) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&fixed) {
            return parse_tool_call_from_value(&value);
        }
    }

    None
}

/// Try to fix common JSON errors where LLMs output unescaped quotes in code strings
fn try_fix_malformed_json(json_str: &str) -> Option<String> {
    // Look for pattern: "code": "...unescaped quotes..."
    // The issue is usually in the code field where quotes aren't escaped

    // Simple heuristic: find "code": " and extract until the closing }}
    let code_marker = "\"code\": \"";
    let code_start = json_str.find(code_marker)?;
    let code_value_start = code_start + code_marker.len();

    // Find the end - look for "}} or "}  } patterns that close the arguments object
    // We need to find where the code value ends, which is before the closing braces
    let remaining = &json_str[code_value_start..];

    // Find the last "}} or similar pattern that closes the JSON
    let end_pattern = remaining.rfind("\"}}")?;

    // The actual code content is from code_value_start to end_pattern
    let code_content = &remaining[..end_pattern];

    // Escape any unescaped double quotes in the code content
    let escaped_code = code_content
        .replace("\\\"", "\u{FFFF}") // Temporarily replace already-escaped quotes
        .replace("\"", "\\\"") // Escape all double quotes
        .replace("\u{FFFF}", "\\\""); // Restore already-escaped quotes

    // Reconstruct the JSON
    let prefix = &json_str[..code_value_start];
    let suffix = &json_str[code_value_start + end_pattern..];

    Some(format!("{}{}{}", prefix, escaped_code, suffix))
}

/// Parse a tool call from a JSON Value
fn parse_tool_call_from_value(value: &serde_json::Value) -> Option<ToolCall> {
    // Check for the expected format: {"name": "...", "arguments": {...}}
    let name = value.get("name").and_then(|n| n.as_str())?;

    // Handle different argument formats
    let arguments = if let Some(args) = value.get("arguments") {
        args.clone()
    } else if let Some(params) = value.get("parameters") {
        // Some models use "parameters" instead of "arguments"
        // Handle the malformed format where schema is mixed with values
        if let Some(code_obj) = params.get("code") {
            if code_obj.is_object() {
                // Malformed: {"parameters": {"code": {"value": "..."}}}
                let code = code_obj
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let language = params
                    .get("language")
                    .and_then(|l| {
                        if l.is_string() {
                            l.as_str()
                        } else {
                            l.get("value").and_then(|v| v.as_str())
                        }
                    })
                    .unwrap_or("python");
                serde_json::json!({
                    "language": language,
                    "code": code
                })
            } else {
                params.clone()
            }
        } else {
            params.clone()
        }
    } else {
        return None;
    };

    Some(ToolCall {
        function: FunctionCall {
            name: name.to_string(),
            arguments,
        },
    })
}
