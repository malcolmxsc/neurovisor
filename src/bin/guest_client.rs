use std::env;
use std::collections::HashMap;
use tonic::transport::Endpoint;
use tower::service_fn;
use hyper_util::rt::tokio::TokioIo;
use neurovisor::grpc::inference::inference_service_client::InferenceServiceClient;
use neurovisor::grpc::inference::InferenceRequest;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let prompt = env::args().nth(1).unwrap_or_else(||"Hello what is AI?".to_string());

    // Connect to host via vsock
    // CID 2 is always the host from the guest's perspective
    // Port 6000 is what the host is listening on
    println!("[GUEST] Connecting to host via vsock...");
    let channel = Endpoint::try_from("http://[::1]:6000")?
        .connect_with_connector(service_fn(|_| async {
            println!("[GUEST] Opening vsock connection to CID 2, port 6000...");
            let stream = tokio_vsock::VsockStream::connect(2, 6000).await?;
            Ok::<_, std::io::Error>(TokioIo::new(stream))
        }))
        .await?;

    println!("[GUEST] ✓ Connected to host!");

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