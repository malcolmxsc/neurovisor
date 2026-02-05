//! Agent Controller - main orchestration loop for LLM-driven code execution
//!
//! The AgentController manages the interaction between Ollama and the VM pool,
//! executing code in sandboxed VMs and feeding results back to the LLM.

use std::sync::Arc;

use tracing::{info, info_span, warn, Instrument};
use uuid::Uuid;

use crate::metrics::{
    // Aggregate metrics (persistent, for dashboards)
    AGENT_TASKS, AGENT_ITERATIONS_TOTAL, AGENT_TOOL_CALLS,
    CODE_EXECUTIONS, CODE_EXECUTION_DURATION_TOTAL, MODEL_LOAD_TIME,
    LLM_CALL_TIME,
    // Per-trace metrics (ephemeral, for correlation/debugging)
    AGENT_ITERATIONS, AGENT_TASKS_TOTAL, AGENT_TOOL_CALLS_TOTAL,
    CODE_EXECUTIONS_TOTAL, CODE_EXECUTION_DURATION, MODEL_LOAD_DURATION,
    LLM_CALL_DURATION,
};

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
            connection_retries: 20,
            connection_retry_delay_ms: 250,
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
    /// Time for first LLM call in milliseconds (includes model load time)
    pub model_load_time_ms: Option<f64>,
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

        // Create root span for the entire agent task
        let root_span = info_span!(
            "agent_task",
            trace_id = %trace_id,
            task = %task,
            model = %self.config.model,
            otel.name = "agent_task"
        );

        async {
            info!(trace_id = %trace_id, task = %task, "Starting agent task");
            println!("[AGENT] Trace ID: {}", trace_id);

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
            let mut model_load_time_ms: Option<f64> = None;

            loop {
                iterations += 1;

                if iterations > self.config.max_iterations {
                    warn!(trace_id = %trace_id, iterations, "Max iterations reached");
                    // Aggregate metrics (persistent)
                    AGENT_TASKS.with_label_values(&["max_iterations"]).inc();
                    AGENT_ITERATIONS_TOTAL.observe(iterations as f64);
                    // Per-trace metrics (for correlation)
                    AGENT_TASKS_TOTAL.with_label_values(&["max_iterations", &trace_id]).inc();
                    AGENT_ITERATIONS.with_label_values(&[&trace_id]).observe(iterations as f64);
                    return Err(AgentError::MaxIterationsReached);
                }

                // Create span for LLM call
                let llm_span = info_span!(
                    "llm_call",
                    trace_id = %trace_id,
                    iteration = iterations,
                    model = %self.config.model,
                    otel.name = "llm_call"
                );

                let is_first_call = iterations == 1;
                if is_first_call {
                    info!(trace_id = %trace_id, model = %self.config.model, "First LLM call (includes model load)");
                    println!("[AGENT] Calling {} (first call includes model load time)...", self.config.model);
                }
                let call_start = std::time::Instant::now();

                let response = self
                    .chat_client
                    .chat(messages.clone(), &self.config.model, Some(tools.clone()))
                    .instrument(llm_span)
                    .await?;

                let call_duration_ms = call_start.elapsed().as_secs_f64() * 1000.0;
                let call_duration_secs = call_duration_ms / 1000.0;

                // Record LLM call duration - aggregate and per-trace
                LLM_CALL_TIME.with_label_values(&[&self.config.model]).observe(call_duration_secs);
                LLM_CALL_DURATION.with_label_values(&[&self.config.model, &trace_id]).observe(call_duration_secs);

                if is_first_call {
                    model_load_time_ms = Some(call_duration_ms);
                    // Aggregate and per-trace model load metrics
                    MODEL_LOAD_TIME.with_label_values(&[&self.config.model]).observe(call_duration_secs);
                    MODEL_LOAD_DURATION.with_label_values(&[&self.config.model, &trace_id]).observe(call_duration_secs);
                    info!(trace_id = %trace_id, duration_ms = call_duration_ms, "Model loaded");
                    println!("[AGENT] First LLM call completed in {:.2}ms (includes model load)", call_duration_ms);
                } else {
                    info!(trace_id = %trace_id, iteration = iterations, duration_ms = call_duration_ms, "LLM call completed");
                    println!("[AGENT] LLM call {} completed in {:.2}ms", iterations, call_duration_ms);
                }

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
                    info!(trace_id = %trace_id, iterations, tool_calls = tool_calls_made, "Agent task completed");
                    // Aggregate metrics (persistent)
                    AGENT_TASKS.with_label_values(&["success"]).inc();
                    AGENT_ITERATIONS_TOTAL.observe(iterations as f64);
                    // Per-trace metrics (for correlation)
                    AGENT_TASKS_TOTAL.with_label_values(&["success", &trace_id]).inc();
                    AGENT_ITERATIONS.with_label_values(&[&trace_id]).observe(iterations as f64);

                    return Ok(AgentResult {
                        final_response: response.message.content,
                        iterations,
                        tool_calls_made,
                        execution_records,
                        trace_id,
                        model_load_time_ms,
                    });
                }

                for tool_call in tool_calls {
                    if tool_call.function.name == "execute_code" {
                        tool_calls_made += 1;
                        // Aggregate and per-trace tool call metrics
                        AGENT_TOOL_CALLS.with_label_values(&["execute_code"]).inc();
                        AGENT_TOOL_CALLS_TOTAL.with_label_values(&["execute_code", &trace_id]).inc();

                        // Parse arguments
                        let args = &tool_call.function.arguments;
                        let language = args["language"].as_str().unwrap_or("bash");
                        let code = args["code"].as_str().unwrap_or("");

                        // Validate: reject empty code
                        if code.trim().is_empty() {
                            warn!(trace_id = %trace_id, "Rejecting empty code from LLM");
                            println!("[AGENT] ⚠️ Rejecting empty code from LLM");
                            messages.push(ChatMessage::tool(
                                "Error: code parameter is empty. Please provide actual code to execute.".to_string()
                            ));
                            continue;
                        }

                        // Execute in VM with tracing span
                        let exec_span = info_span!(
                            "code_execution",
                            trace_id = %trace_id,
                            language = %language,
                            code_len = code.len(),
                            otel.name = "code_execution"
                        );

                        info!(trace_id = %trace_id, language, code_len = code.len(), "Executing code");
                        println!("[AGENT] Executing {} code:", language);
                        println!("┌─────────────────────────────────────────");
                        for line in code.lines() {
                            println!("│ {}", line);
                        }
                        println!("└─────────────────────────────────────────");

                        let result = self.execute_code(language, code, &trace_id)
                            .instrument(exec_span)
                            .await;

                        // Format result for Ollama
                        let tool_response = match &result {
                            Ok(record) => {
                                info!(
                                    trace_id = %trace_id,
                                    exit_code = record.exit_code,
                                    duration_ms = record.duration_ms,
                                    "Code execution succeeded"
                                );
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
                                warn!(trace_id = %trace_id, error = %e, "Code execution failed");
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
        .instrument(root_span)
        .await
    }

    /// Execute code in an isolated VM with streaming output
    ///
    /// The trace_id is propagated to the guest via the `NEUROVISOR_TRACE_ID` environment variable,
    /// enabling distributed tracing across host-guest boundaries.
    async fn execute_code(
        &self,
        language: &str,
        code: &str,
        trace_id: &str,
    ) -> Result<ExecutionRecord, AgentError> {
        // Acquire VM from pool with trace correlation
        let vm = self
            .pool
            .acquire(Some(trace_id))
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
                self.pool.release(vm).await;
                return Err(AgentError::ConnectionFailed(e.to_string()));
            }
        };

        // Execute code and display output
        println!("[OUTPUT] ─────────────────────────────────────");

        // Propagate trace_id to guest via environment variable
        let mut env = std::collections::HashMap::new();
        env.insert("NEUROVISOR_TRACE_ID".to_string(), trace_id.to_string());

        let result = client
            .execute_with_env(language, code, self.config.execution_timeout_secs, env)
            .await;

        // Display output
        if let Ok(ref response) = result {
            for line in response.stdout.lines() {
                println!("[stdout] {}", line);
            }
            for line in response.stderr.lines() {
                println!("[stderr] {}", line);
            }
            println!("[OUTPUT] ─────────────────────────────────────");
            if response.timed_out {
                println!("[OUTPUT] Timed out after {:.2}ms", response.duration_ms);
            } else {
                println!("[OUTPUT] Completed in {:.2}ms (exit: {})", response.duration_ms, response.exit_code);
            }
        }

        // Release VM
        self.pool.release(vm).await;

        // Convert result and record metrics with trace_id
        match result {
            Ok(streaming_result) => {
                let duration_secs = streaming_result.duration_ms / 1000.0;
                let status = if streaming_result.timed_out {
                    "timeout"
                } else if streaming_result.exit_code == 0 {
                    "success"
                } else {
                    "error"
                };
                // Aggregate metrics (persistent)
                CODE_EXECUTION_DURATION_TOTAL.with_label_values(&[language]).observe(duration_secs);
                CODE_EXECUTIONS.with_label_values(&[language, status]).inc();
                // Per-trace metrics (for correlation)
                CODE_EXECUTION_DURATION.with_label_values(&[language, trace_id]).observe(duration_secs);
                CODE_EXECUTIONS_TOTAL.with_label_values(&[language, status, trace_id]).inc();

                Ok(ExecutionRecord {
                    language: language.to_string(),
                    code: code.to_string(),
                    stdout: streaming_result.stdout,
                    stderr: streaming_result.stderr,
                    exit_code: streaming_result.exit_code,
                    duration_ms: streaming_result.duration_ms,
                    timed_out: streaming_result.timed_out,
                })
            }
            Err(e) => {
                // Aggregate and per-trace error metrics
                CODE_EXECUTIONS.with_label_values(&[language, "error"]).inc();
                CODE_EXECUTIONS_TOTAL.with_label_values(&[language, "error", trace_id]).inc();
                Err(AgentError::ExecutionFailed(e.to_string()))
            }
        }
    }
}
