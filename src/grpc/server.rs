use tonic::{Request,Response,Status};
use tokio_stream::wrappers::ReceiverStream;
use futures_util::StreamExt;
use uuid::Uuid;
use crate::ollama::{OllamaClient, StreamChunk};
use crate::metrics::{
    REQUESTS_TOTAL, INFERENCE_DURATION, TOKENS_GENERATED_TOTAL,
    ERRORS_TOTAL, REQUESTS_IN_FLIGHT, REQUEST_SIZE_BYTES, GRPC_REQUEST_DURATION,
};

// Include the generated proto code

pub mod inference {
    tonic::include_proto!("neurovisor.inference");

}

use inference::inference_service_server::InferenceService;
use inference::{InferenceRequest, InferenceResponse,TokenChunk,InferenceMetadata};

pub struct InferenceServer {
    ollama: OllamaClient,
}

impl InferenceServer {
    pub fn new(ollama: OllamaClient) -> Self {
        Self {ollama}
    }
}


#[tonic::async_trait]
impl InferenceService for InferenceServer {
    type InferStreamStream = ReceiverStream<Result<TokenChunk, Status>>;

    async fn infer_stream(
        &self,
        request: Request<InferenceRequest>,
    ) -> Result<Response<Self::InferStreamStream>, Status> {
        // Start timing for gRPC duration
        let start = std::time::Instant::now();

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

        // Generate a unique trace ID for this request
        let trace_id = Uuid::now_v7().to_string();

        let mut token_stream = match self.ollama.generate_stream(&prompt, &model).await {
            Ok(s) => s,
            Err(e) => {
                ERRORS_TOTAL.with_label_values(&["ollama_error"]).inc();
                REQUESTS_IN_FLIGHT.dec();
                GRPC_REQUEST_DURATION.with_label_values(&["infer_stream"]).observe(start.elapsed().as_secs_f64());
                return Err(Status::internal(e.to_string()));
            }
        };

        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let model_clone = model.clone();
        let trace_id_clone = trace_id;

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
                                // Receiver dropped, clean up metrics
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
                        return;
                    }

                    Err(e) => {
                        ERRORS_TOTAL.with_label_values(&["stream_error"]).inc();
                        REQUESTS_IN_FLIGHT.dec();
                        GRPC_REQUEST_DURATION.with_label_values(&["infer_stream"]).observe(start.elapsed().as_secs_f64());
                        let _ = tx.send(Err(Status::internal(e.to_string()))).await;
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
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn infer(
        &self,
        request: Request<InferenceRequest>,
     ) -> Result<Response<InferenceResponse>, Status> {
        // Start timing for gRPC duration
        let start = std::time::Instant::now();

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

        // Generate trace ID for this request
        let trace_id = Uuid::now_v7().to_string();

        let result = match self.ollama.generate(&prompt, &model).await {
            Ok(r) => r,
            Err(e) => {
                // Record error and clean up in-flight counter
                ERRORS_TOTAL.with_label_values(&["ollama_error"]).inc();
                REQUESTS_IN_FLIGHT.dec();
                GRPC_REQUEST_DURATION.with_label_values(&["infer"]).observe(start.elapsed().as_secs_f64());
                return Err(Status::internal(e.to_string()));
            }
        };

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