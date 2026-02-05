//! Integration tests for the agent system
//!
//! These tests verify the agent controller, tool calling, and code execution flow.
//! Some tests require Ollama to be running and are marked #[ignore].

use neurovisor::agent::AgentConfig;
use neurovisor::ollama::tool_use::{ChatMessage, Tool, ToolFunction};

/// Test that AgentConfig has sensible defaults
#[test]
fn test_agent_config_defaults() {
    let config = AgentConfig::default();

    assert_eq!(config.model, "qwen3");
    assert_eq!(config.max_iterations, 10);
    assert_eq!(config.execution_timeout_secs, 30);
    assert!(config.connection_retries > 0);
    assert!(config.connection_retry_delay_ms > 0);
}

/// Test that AgentConfig can be customized
#[test]
fn test_agent_config_custom() {
    let config = AgentConfig {
        model: "codellama".to_string(),
        max_iterations: 5,
        execution_timeout_secs: 60,
        vsock_port: 7000,
        connection_retries: 10,
        connection_retry_delay_ms: 500,
        system_prompt: Some("You are a helpful coding assistant.".to_string()),
    };

    assert_eq!(config.model, "codellama");
    assert_eq!(config.max_iterations, 5);
    assert_eq!(config.execution_timeout_secs, 60);
    assert_eq!(config.vsock_port, 7000);
}

/// Test ChatMessage construction with helper methods
#[test]
fn test_chat_message_construction() {
    let user_msg = ChatMessage::user("Hello, world!");
    assert_eq!(user_msg.role, "user");
    assert_eq!(user_msg.content, "Hello, world!");
    assert!(user_msg.tool_calls.is_none());

    let system_msg = ChatMessage::system("You are helpful.");
    assert_eq!(system_msg.role, "system");

    let assistant_msg = ChatMessage::assistant("I can help!");
    assert_eq!(assistant_msg.role, "assistant");

    let tool_msg = ChatMessage::tool("Result: 42");
    assert_eq!(tool_msg.role, "tool");
}

/// Test Tool definition for code execution
#[test]
fn test_execute_code_tool_definition() {
    let tool = Tool {
        tool_type: "function".to_string(),
        function: ToolFunction {
            name: "execute_code".to_string(),
            description: "Execute code in a sandboxed VM".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "description": "Programming language",
                        "enum": ["python", "bash", "javascript", "go", "rust"]
                    },
                    "code": {
                        "type": "string",
                        "description": "Code to execute"
                    }
                },
                "required": ["language", "code"]
            }),
        },
    };

    assert_eq!(tool.tool_type, "function");
    assert_eq!(tool.function.name, "execute_code");

    // Verify parameters schema
    let params = &tool.function.parameters;
    assert_eq!(params["type"], "object");
    assert!(params["properties"]["language"].is_object());
    assert!(params["properties"]["code"].is_object());
    assert_eq!(params["required"][0], "language");
    assert_eq!(params["required"][1], "code");

    // Check languages in enum
    let languages = params["properties"]["language"]["enum"].as_array().unwrap();
    assert!(languages.iter().any(|v| v == "python"));
    assert!(languages.iter().any(|v| v == "go"));
    assert!(languages.iter().any(|v| v == "rust"));
}

/// Test Tool serialization to JSON
#[test]
fn test_tool_serialization() {
    let tool = Tool {
        tool_type: "function".to_string(),
        function: ToolFunction {
            name: "test_func".to_string(),
            description: "A test function".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
    };

    let json = serde_json::to_string(&tool).unwrap();
    assert!(json.contains("\"type\":\"function\""));
    assert!(json.contains("\"name\":\"test_func\""));
}

/// Test ChatMessage serialization
#[test]
fn test_chat_message_serialization() {
    let msg = ChatMessage::user("test message");
    let json = serde_json::to_string(&msg).unwrap();

    assert!(json.contains("\"role\":\"user\""));
    assert!(json.contains("\"content\":\"test message\""));
    // tool_calls should be skipped when None
    assert!(!json.contains("tool_calls"));
}

// Integration tests that require external services

/// Test agent with a simple task (requires Ollama + VMs)
#[test]
#[ignore = "Requires Ollama and Firecracker VMs running"]
fn test_agent_simple_task() {
    // This test would:
    // 1. Create a VMPool
    // 2. Create AgentController
    // 3. Run a simple task like "print hello world"
    // 4. Verify the result contains expected output
}

/// Test agent handles tool call errors gracefully
#[test]
#[ignore = "Requires Ollama and Firecracker VMs running"]
fn test_agent_tool_call_error_handling() {
    // This test would verify that:
    // 1. Agent handles invalid tool calls gracefully
    // 2. Agent handles execution errors gracefully
    // 3. Agent can recover and retry
}

/// Test agent respects max_iterations limit
#[test]
#[ignore = "Requires Ollama and Firecracker VMs running"]
fn test_agent_max_iterations() {
    // This test would verify that:
    // 1. Agent stops after max_iterations
    // 2. Result indicates iteration limit was reached
}
