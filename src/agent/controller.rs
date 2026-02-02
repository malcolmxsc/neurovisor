//! Agent Controller - main orchestration loop for LLM-driven code execution
//!
//! The AgentController manages the interaction between Ollama and the VM pool,
//! executing code in sandboxed VMs and feeding results back to the LLM.

use std::sync::Arc;

use uuid::Uuid;

use crate::grpc::{ExecutionClient, ExecutionError};
use crate::ollama::{
    parse_tool_calls_from_text, ChatClient, ChatError, ChatMessage, DEFAULT_AGENT_SYSTEM_PROMPT,
};
use crate::vm::VMPool;

/// Configuration for the agent controller
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Model to use for Ollama (e.g., "llama3.2")
    pub model: String,
    /// Maximum number of iterations (LLM calls) before stopping
    pub max_iterations: usize,
    /// Timeout for code execution in seconds
    pub execution_timeout_secs: u32,
    /// Custom system prompt (uses default if None)
    pub system_prompt: Option<String>,
    /// Port for vsock connections to guest execution service
    pub vsock_port: u32,
    /// Maximum retries for connecting to guest execution service
    pub connection_retries: u32,
    /// Delay between connection retries in milliseconds
    pub connection_retry_delay_ms: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: "qwen3".to_string(),
            max_iterations: 10,
            execution_timeout_secs: 30,
            system_prompt: None,
            vsock_port: 6000,
            connection_retries: 10,
            connection_retry_delay_ms: 500,
        }
    }
}

/// Result of an agent run
#[derive(Debug, Clone)]
pub struct AgentResult {
    /// Final response from the LLM
    pub final_response: String,
    /// Number of iterations (LLM calls) made
    pub iterations: usize,
    /// Number of tool calls executed
    pub tool_calls_made: usize,
    /// Records of all code executions
    pub execution_records: Vec<ExecutionRecord>,
    /// Unique trace ID for this agent run
    pub trace_id: String,
}

/// Record of a single code execution
#[derive(Debug, Clone)]
pub struct ExecutionRecord {
    /// Programming language used
    pub language: String,
    /// Code that was executed
    pub code: String,
    /// Standard output from execution
    pub stdout: String,
    /// Standard error from execution
    pub stderr: String,
    /// Exit code of the process
    pub exit_code: i32,
    /// Execution duration in milliseconds
    pub duration_ms: f64,
    /// Whether execution timed out
    pub timed_out: bool,
}

/// Error type for agent operations
#[derive(Debug)]
pub enum AgentError {
    /// Maximum iterations reached without completing the task
    MaxIterationsReached,
    /// Failed to acquire a VM from the pool
    VmAcquisitionFailed(String),
    /// Failed to connect to the guest execution service
    ConnectionFailed(String),
    /// Code execution failed
    ExecutionFailed(String),
    /// Ollama chat error
    OllamaError(ChatError),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::MaxIterationsReached => write!(f, "Maximum iterations reached"),
            AgentError::VmAcquisitionFailed(msg) => write!(f, "Failed to acquire VM: {}", msg),
            AgentError::ConnectionFailed(msg) => {
                write!(f, "Failed to connect to guest: {}", msg)
            }
            AgentError::ExecutionFailed(msg) => write!(f, "Execution failed: {}", msg),
            AgentError::OllamaError(e) => write!(f, "Ollama error: {}", e),
        }
    }
}

impl std::error::Error for AgentError {}

impl From<ChatError> for AgentError {
    fn from(e: ChatError) -> Self {
        AgentError::OllamaError(e)
    }
}

impl From<ExecutionError> for AgentError {
    fn from(e: ExecutionError) -> Self {
        AgentError::ExecutionFailed(e.to_string())
    }
}

/// Agent Controller orchestrating LLM and code execution
pub struct AgentController {
    chat_client: ChatClient,
    pool: Arc<VMPool>,
    config: AgentConfig,
}

impl AgentController {
    /// Create a new agent controller
    ///
    /// # Arguments
    /// * `chat_client` - Ollama chat client for LLM interactions
    /// * `pool` - VM pool for sandboxed code execution
    /// * `config` - Agent configuration
    pub fn new(chat_client: ChatClient, pool: Arc<VMPool>, config: AgentConfig) -> Self {
        Self {
            chat_client,
            pool,
            config,
        }
    }

    /// Run the agent loop for a given task
    ///
    /// # Arguments
    /// * `task` - The user's task/prompt to accomplish
    ///
    /// # Returns
    /// AgentResult containing the final response and execution history
    pub async fn run(&self, task: &str) -> Result<AgentResult, AgentError> {
        let trace_id = Uuid::now_v7().to_string();
        let tools = vec![ChatClient::execute_code_tool()];

        // Initialize conversation
        let mut messages = vec![];

        // Add system prompt
        let system_prompt = self
            .config
            .system_prompt
            .clone()
            .unwrap_or_else(|| DEFAULT_AGENT_SYSTEM_PROMPT.to_string());
        messages.push(ChatMessage::system(system_prompt));

        // Add user task
        messages.push(ChatMessage::user(task));

        let mut iterations = 0;
        let mut tool_calls_made = 0;
        let mut execution_records = vec![];

        loop {
            iterations += 1;

            if iterations > self.config.max_iterations {
                return Err(AgentError::MaxIterationsReached);
            }

            // Call Ollama with tools
            let response = self
                .chat_client
                .chat(messages.clone(), &self.config.model, Some(tools.clone()))
                .await?;

            // Add assistant response to history
            messages.push(response.message.clone());

            // Check for tool calls - try native format first, then fallback to text parsing
            let tool_calls = response
                .message
                .tool_calls
                .clone()
                .filter(|tc| !tc.is_empty())
                .unwrap_or_else(|| parse_tool_calls_from_text(&response.message.content));

            if tool_calls.is_empty() {
                // No tool calls - model is done
                return Ok(AgentResult {
                    final_response: response.message.content,
                    iterations,
                    tool_calls_made,
                    execution_records,
                    trace_id,
                });
            }

            for tool_call in tool_calls {
                if tool_call.function.name == "execute_code" {
                    tool_calls_made += 1;

                    // Parse arguments
                    let args = &tool_call.function.arguments;
                    let language = args["language"].as_str().unwrap_or("bash");
                    let code = args["code"].as_str().unwrap_or("");

                    // Execute in VM
                    println!("[AGENT] Executing {} code:", language);
                    println!("┌─────────────────────────────────────────");
                    for line in code.lines() {
                        println!("│ {}", line);
                    }
                    println!("└─────────────────────────────────────────");
                    let result = self.execute_code(language, code).await;

                    // Format result for Ollama
                    let tool_response = match &result {
                        Ok(record) => {
                            println!("[AGENT] ✅ Execution succeeded (exit code: {})", record.exit_code);
                            execution_records.push(record.clone());
                            format!(
                                "Exit code: {}\nStdout:\n{}\nStderr:\n{}{}",
                                record.exit_code,
                                record.stdout,
                                record.stderr,
                                if record.timed_out {
                                    "\n(Execution timed out)"
                                } else {
                                    ""
                                }
                            )
                        }
                        Err(e) => {
                            println!("[AGENT] ❌ Execution failed: {}", e);
                            format!("Error: {}", e)
                        }
                    };

                    // Add tool response to conversation
                    messages.push(ChatMessage::tool(tool_response));
                }
            }
        }
    }

    /// Execute code in an isolated VM
    async fn execute_code(
        &self,
        language: &str,
        code: &str,
    ) -> Result<ExecutionRecord, AgentError> {
        // Acquire VM from pool
        let vm = self
            .pool
            .acquire()
            .await
            .map_err(|e| AgentError::VmAcquisitionFailed(e.to_string()))?;

        let vsock_path = vm.vsock_listener_path(self.config.vsock_port);

        // Connect to guest execution service with retry
        let client_result = ExecutionClient::connect_with_retry(
            vsock_path.clone(),
            self.config.connection_retries,
            self.config.connection_retry_delay_ms,
        )
        .await;

        let mut client = match client_result {
            Ok(c) => c,
            Err(e) => {
                // Release VM on connection failure
                self.pool.release(vm).await;
                return Err(AgentError::ConnectionFailed(e.to_string()));
            }
        };

        // Execute code
        let result = client
            .execute(language, code, self.config.execution_timeout_secs)
            .await;

        // Release VM (always, even on error)
        self.pool.release(vm).await;

        // Convert result
        match result {
            Ok(response) => Ok(ExecutionRecord {
                language: language.to_string(),
                code: code.to_string(),
                stdout: response.stdout,
                stderr: response.stderr,
                exit_code: response.exit_code,
                duration_ms: response.duration_ms,
                timed_out: response.timed_out,
            }),
            Err(e) => Err(AgentError::ExecutionFailed(e.to_string())),
        }
    }
}
