//! Gateway Server - routes inference requests through the VM pool
//!
//! The GatewayServer receives external inference requests, acquires a VM from
//! the pool, executes the request, and returns the response. Each request gets
//! its own isolated VM for security.
//!
//! # Request Flow
//!
//! ```text
//! Client ──request──► Gateway ──acquire──► VMPool
//!                        │                    │
//!                        │◄───── VM ──────────┘
//!                        │
//!                        ├──execute on VM──► Ollama
//!                        │
//!                        ├──release──► VMPool (destroy + replenish)
//!                        │
//! Client ◄──response─────┘
//! ```

use std::sync::Arc;
use tonic::{Request, Response, Status};
use tokio_stream::wrappers::ReceiverStream;
use futures_util::StreamExt;
use uuid::Uuid;

use crate::ollama::{OllamaClient, StreamChunk};
use crate::security::RateLimiter;
use crate::vm::VMPool;
use crate::metrics::{
    REQUESTS_TOTAL, INFERENCE_DURATION, TOKENS_GENERATED_TOTAL,
    ERRORS_TOTAL, REQUESTS_IN_FLIGHT, REQUEST_SIZE_BYTES, GRPC_REQUEST_DURATION,
};

use super::server::inference::inference_service_server::InferenceService;
use super::server::inference::{InferenceRequest, InferenceResponse, TokenChunk, InferenceMetadata};

/// Gateway server that routes requests through the VM pool
pub struct GatewayServer {
    /// VM pool for acquiring/releasing VMs
    pool: Arc<VMPool>,
    /// Rate limiter to prevent abuse
    rate_limiter: Arc<RateLimiter>,
    /// Ollama client for inference (shared across requests)
    ollama: OllamaClient,
}

impl GatewayServer {
    /// Create a new gateway server
    pub fn new(
        pool: Arc<VMPool>,
        rate_limiter: Arc<RateLimiter>,
        ollama: OllamaClient,
    ) -> Self {
        Self {
            pool,
            rate_limiter,
            ollama,
        }
    }
}

#[tonic::async_trait]
impl InferenceService for GatewayServer {
    type InferStreamStream = ReceiverStream<Result<TokenChunk, Status>>;

    async fn infer_stream(
        &self,
        request: Request<InferenceRequest>,
    ) -> Result<Response<Self::InferStreamStream>, Status> {
        // Check rate limit before processing
        if !self.rate_limiter.try_acquire() {
            ERRORS_TOTAL.with_label_values(&["rate_limited"]).inc();
            return Err(Status::resource_exhausted("Rate limit exceeded"));
        }

        // Start timing for gRPC duration
        let start = std::time::Instant::now();

        // Extract trace_id from gRPC metadata (supports x-trace-id header)
        let incoming_trace_id = request
            .metadata()
            .get("x-trace-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let req = request.into_inner();
        let prompt = req.prompt;
        let model = if req.model.is_empty() {
            "llama3.2".to_string()
        } else {
            req.model
        };

        // Record metrics at request start
        REQUESTS_TOTAL.with_label_values(&[&model]).inc();
        REQUESTS_IN_FLIGHT.inc();
        REQUEST_SIZE_BYTES.observe(prompt.len() as f64);

        // Use incoming trace_id from metadata or generate a new one
        let trace_id = incoming_trace_id.unwrap_or_else(|| Uuid::now_v7().to_string());

        // Acquire a VM from the pool with trace correlation
        let vm = match self.pool.acquire(Some(&trace_id)).await {
            Ok(v) => v,
            Err(e) => {
                ERRORS_TOTAL.with_label_values(&["no_vm_available"]).inc();
                REQUESTS_IN_FLIGHT.dec();
                GRPC_REQUEST_DURATION.with_label_values(&["infer_stream"]).observe(start.elapsed().as_secs_f64());
                return Err(Status::unavailable(format!("No VM available: {}", e)));
            }
        };

        let vm_id = vm.vm_id.clone();
        println!("[INFO] Request using VM {} (CID: {}, trace: {})", vm_id, vm.cid, trace_id);

        // Execute inference
        let mut token_stream = match self.ollama.generate_stream(&prompt, &model, Some(&trace_id)).await {
            Ok(s) => s,
            Err(e) => {
                // Release VM on error
                self.pool.release(vm).await;
                ERRORS_TOTAL.with_label_values(&["ollama_error"]).inc();
                REQUESTS_IN_FLIGHT.dec();
                GRPC_REQUEST_DURATION.with_label_values(&["infer_stream"]).observe(start.elapsed().as_secs_f64());
                return Err(Status::internal(e.to_string()));
            }
        };

        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let model_clone = model.clone();
        let trace_id_clone = trace_id;
        let pool_clone = Arc::clone(&self.pool);

        tokio::spawn(async move {
            let trace_id = trace_id_clone;
            let mut index = 0;

            while let Some(chunk_result) = token_stream.next().await {
                match chunk_result {
                    Ok(StreamChunk::Token(token)) => {
                        if !token.is_empty() {
                            let chunk = TokenChunk {
                                token,
                                is_final: false,
                                token_index: index,
                                metadata: None,
                            };
                            if tx.send(Ok(chunk)).await.is_err() {
                                // Receiver dropped, clean up
                                pool_clone.release(vm).await;
                                REQUESTS_IN_FLIGHT.dec();
                                GRPC_REQUEST_DURATION.with_label_values(&["infer_stream"]).observe(start.elapsed().as_secs_f64());
                                return;
                            }
                            index += 1;
                        }
                    }

                    Ok(StreamChunk::Done(metadata)) => {
                        let latency_ms = (metadata.eval_duration_ns as f64) / 1_000_000.0;

                        // Record success metrics
                        INFERENCE_DURATION.observe(latency_ms / 1000.0);
                        TOKENS_GENERATED_TOTAL.with_label_values(&[&model_clone]).inc_by(metadata.eval_count as f64);
                        REQUESTS_IN_FLIGHT.dec();
                        GRPC_REQUEST_DURATION.with_label_values(&["infer_stream"]).observe(start.elapsed().as_secs_f64());

                        let final_chunk = TokenChunk {
                            token: String::new(),
                            is_final: true,
                            token_index: index,
                            metadata: Some(InferenceMetadata {
                                total_tokens: metadata.eval_count as i32,
                                total_latency_ms: latency_ms,
                                model: model_clone,
                                trace_id: trace_id.clone(),
                            }),
                        };
                        let _ = tx.send(Ok(final_chunk)).await;

                        // Release VM after completion
                        pool_clone.release(vm).await;
                        return;
                    }

                    Err(e) => {
                        ERRORS_TOTAL.with_label_values(&["stream_error"]).inc();
                        REQUESTS_IN_FLIGHT.dec();
                        GRPC_REQUEST_DURATION.with_label_values(&["infer_stream"]).observe(start.elapsed().as_secs_f64());
                        let _ = tx.send(Err(Status::internal(e.to_string()))).await;

                        // Release VM on error
                        pool_clone.release(vm).await;
                        return;
                    }
                }
            }

            // Fallback: stream ended without Done chunk
            REQUESTS_IN_FLIGHT.dec();
            GRPC_REQUEST_DURATION.with_label_values(&["infer_stream"]).observe(start.elapsed().as_secs_f64());

            let final_chunk = TokenChunk {
                token: String::new(),
                is_final: true,
                token_index: index,
                metadata: Some(InferenceMetadata {
                    total_tokens: index,
                    total_latency_ms: 0.0,
                    model: model_clone,
                    trace_id,
                }),
            };
            let _ = tx.send(Ok(final_chunk)).await;

            // Release VM
            pool_clone.release(vm).await;
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn infer(
        &self,
        request: Request<InferenceRequest>,
    ) -> Result<Response<InferenceResponse>, Status> {
        // Check rate limit before processing
        if !self.rate_limiter.try_acquire() {
            ERRORS_TOTAL.with_label_values(&["rate_limited"]).inc();
            return Err(Status::resource_exhausted("Rate limit exceeded"));
        }

        // Start timing for gRPC duration
        let start = std::time::Instant::now();

        // Extract trace_id from gRPC metadata (supports x-trace-id header)
        let incoming_trace_id = request
            .metadata()
            .get("x-trace-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let req = request.into_inner();
        let prompt = req.prompt;
        let model = if req.model.is_empty() {
            "llama3.2".to_string()
        } else {
            req.model
        };

        // Record metrics at request start
        REQUESTS_TOTAL.with_label_values(&[&model]).inc();
        REQUESTS_IN_FLIGHT.inc();
        REQUEST_SIZE_BYTES.observe(prompt.len() as f64);

        // Use incoming trace_id from metadata or generate a new one
        let trace_id = incoming_trace_id.unwrap_or_else(|| Uuid::now_v7().to_string());

        // Acquire a VM from the pool with trace correlation
        let vm = match self.pool.acquire(Some(&trace_id)).await {
            Ok(v) => v,
            Err(e) => {
                ERRORS_TOTAL.with_label_values(&["no_vm_available"]).inc();
                REQUESTS_IN_FLIGHT.dec();
                GRPC_REQUEST_DURATION.with_label_values(&["infer"]).observe(start.elapsed().as_secs_f64());
                return Err(Status::unavailable(format!("No VM available: {}", e)));
            }
        };

        let vm_id = vm.vm_id.clone();
        println!("[INFO] Request using VM {} (CID: {}, trace: {})", vm_id, vm.cid, trace_id);

        // Execute inference
        let result = match self.ollama.generate(&prompt, &model, Some(&trace_id)).await {
            Ok(r) => r,
            Err(e) => {
                // Release VM and record error
                self.pool.release(vm).await;
                ERRORS_TOTAL.with_label_values(&["ollama_error"]).inc();
                REQUESTS_IN_FLIGHT.dec();
                GRPC_REQUEST_DURATION.with_label_values(&["infer"]).observe(start.elapsed().as_secs_f64());
                return Err(Status::internal(e.to_string()));
            }
        };

        // Release VM after successful completion
        self.pool.release(vm).await;

        // Convert nanoseconds to milliseconds
        let latency_ms = (result.eval_duration_ns as f64) / 1_000_000.0;

        // Record success metrics
        INFERENCE_DURATION.observe(latency_ms / 1000.0);
        TOKENS_GENERATED_TOTAL.with_label_values(&[&model]).inc_by(result.eval_count as f64);
        REQUESTS_IN_FLIGHT.dec();
        GRPC_REQUEST_DURATION.with_label_values(&["infer"]).observe(start.elapsed().as_secs_f64());

        Ok(Response::new(InferenceResponse {
            response: result.response,
            tokens_generated: result.eval_count as i32,
            latency_ms,
            model_used: model,
            trace_id,
        }))
    }
}
