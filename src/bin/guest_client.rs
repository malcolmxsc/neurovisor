use std::env;
use std::collections::HashMap;
use tonic::transport::Channel;
use neurovisor::grpc::inference::inference_service_client::InferenceServiceClient;
use neurovisor::grpc::inference::InferenceRequest;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let prompt = env::args().nth(1).unwrap_or_else(||"Hello what is AI?".to_string());

    let channel = Channel::from_static("http://localhost:6000")
    .connect()
    .await?;

    let mut client = InferenceServiceClient::new(channel);

    let request = InferenceRequest {
        prompt,
        model: String::new(),
        temperature: 0.7,
        max_tokens: 512,
        stream: false,
        metadata: HashMap::new(),
    };

    let response = client.infer(request).await?;
    let infer_response = response.into_inner();

    println!("Response: {}", infer_response.response);
    println!("Tokens: {}",infer_response.tokens_generated);
    println!("Model: {}",infer_response.model_used);

    Ok(())
}



// 2. create the channel


// 3. create a client

// 4. send the request