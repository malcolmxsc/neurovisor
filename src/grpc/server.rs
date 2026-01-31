use tonic::{Request,Response,Status};
use tokio_stream::wrappers::ReceiverStream;
use futures_util::StreamExt;
use uuid::Uuid;
use crate::ollama::{OllamaClient, StreamChunk};

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
        let req = request.into_inner();
        

        let prompt = req.prompt;
        let model = if req.model.is_empty() {
            "llama3.2".to_string()
        } else {
            req.model
        };

        // Generate a unique trace ID for this request
        // This allows tracking the request across services (host → VM → Ollama → back)
        let trace_id = Uuid::now_v7().to_string();

        let mut token_stream = self.ollama.generate_stream(&prompt,&model).await
            .map_err(|e| Status::internal(e.to_string()))?;
        // sender and receiver
        let (tx,rx) = tokio::sync::mpsc::channel(100);

        let model_clone = model.clone();
        let trace_id_clone = trace_id;
        tokio::spawn(async move {
            // Move trace_id into the async block for use in metadata
            let trace_id = trace_id_clone;
            let mut index = 0;

            // Process each chunk from the Ollama stream
            // Using pattern matching to handle the different StreamChunk variants
            while let Some(chunk_result) = token_stream.next().await {
                match chunk_result {
                    // Pattern: Ok(StreamChunk::Token(token))
                    // This destructures the Result AND the enum in one match arm
                    // - Ok(...) means the Result was successful
                    // - StreamChunk::Token(token) extracts the String from the Token variant
                    Ok(StreamChunk::Token(token)) => {
                        if !token.is_empty() {
                            let chunk = TokenChunk {
                                token,
                                is_final: false,
                                token_index: index,
                                metadata: None,
                            };
                            if tx.send(Ok(chunk)).await.is_err() {
                                // Receiver dropped, stop streaming
                                break;
                            }
                            index += 1;
                        }
                    }

                    // Pattern: Ok(StreamChunk::Done(metadata))
                    // This is the final message from Ollama with timing/count data
                    // - metadata is a GenerateResponse struct with eval_count, eval_duration_ns, etc.
                    Ok(StreamChunk::Done(metadata)) => {
                        // Convert nanoseconds to milliseconds for the API response
                        let latency_ms = (metadata.eval_duration_ns as f64) / 1_000_000.0;

                        let final_chunk = TokenChunk {
                            token: String::new(),
                            is_final: true,
                            token_index: index,
                            metadata: Some(InferenceMetadata {
                                // Use actual token count from Ollama instead of our index
                                total_tokens: metadata.eval_count as i32,
                                // Use actual latency instead of hardcoded 0.0
                                total_latency_ms: latency_ms,
                                model: model_clone,
                                // Trace ID for distributed tracing across services
                                trace_id: trace_id.clone(),
                            }),
                        };
                        let _ = tx.send(Ok(final_chunk)).await;
                        // Stream complete, exit the loop
                        return;
                    }

                    // Pattern: Err(e)
                    // Something went wrong parsing or receiving the stream
                    Err(e) => {
                        let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                        return;
                    }
                }
            }

            // Fallback: if the stream ended without a Done chunk (shouldn't happen normally)
            // This handles edge cases where the stream closes unexpectedly
            let final_chunk = TokenChunk {
                token: String::new(),
                is_final: true,
                token_index: index,
                metadata: Some(InferenceMetadata {
                    total_tokens: index,
                    total_latency_ms: 0.0,  // No metadata available
                    model: model_clone,
                    trace_id,  // Still include trace ID even in fallback
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
        let req = request.into_inner();
        let prompt = req.prompt;
        let model = if req.model.is_empty() {
            "llama3.2".to_string()
        } else {
            req.model
        };

        // Generate trace ID for this request
        let trace_id = Uuid::now_v7().to_string();

        let result = self.ollama.generate(&prompt, &model).await
            .map_err(|e| Status::internal(e.to_string()))?;

        // Convert nanoseconds to milliseconds
        let latency_ms = (result.eval_duration_ns as f64) / 1_000_000.0;

        Ok(Response::new(InferenceResponse {
            response: result.response,
            tokens_generated: result.eval_count as i32,
            latency_ms,
            model_used: model,
            trace_id,
        }))
        
        
     }
    
    
}