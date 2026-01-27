use tonic::{Request,Response,Status};
use tokio_stream::wrappers::ReceiverStream;
use futures_util::StreamExt;
use crate::ollama::OllamaClient;

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
        //TODO implement this method

        let req = request.into_inner();
        

        let prompt = req.prompt;
        let model = if req.model.is_empty() {
            "llama3.2".to_string()
        } else {
            req.model
        };
        let mut token_stream = self.ollama.generate_stream(&prompt,&model).await
            .map_err(|e| Status::internal(e.to_string()))?;
        // sender and receiver 
        let (tx,rx) = tokio::sync::mpsc::channel(100);

        let model_clone = model.clone();
        tokio::spawn(async move {
            let mut index = 0;
            while let Some(token_result) = token_stream.next().await {
                match token_result {
                    Ok(token) => {
                        if !token.is_empty() {
                            let chunk = TokenChunk {
                                token,
                                is_final: false,
                                token_index: index,
                                metadata: None,
                            };
                            if tx.send(Ok(chunk)).await.is_err() {
                                break;
                            }
                            index += 1;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                        return;
                    }
                }
            }

            let final_chunk = TokenChunk {
                token: String::new(),
                is_final: true,
                token_index: index,
                metadata: Some(InferenceMetadata {
                    total_tokens: index,
                    total_latency_ms: 0.0,
                    model: model_clone,
                    trace_id: String::new(),
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
    

        // call olama client
        let req = request.into_inner();
        let prompt = req.prompt;
        let model = if req.model.is_empty() {
            "llama3.2".to_string()
        } else {
            req.model
        };

        let result = self.ollama.generate(&prompt,&model).await
        .map_err(|e| Status::internal(e.to_string()))?;

       
        // return the response

        Ok(Response::new(InferenceResponse {
            response: result,
            tokens_generated: 0,
            latency_ms: 0.0,
            model_used: model.to_string(),

        }))
        
        
     }
    
    
}