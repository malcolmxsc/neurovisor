//! Quick test to verify streaming latency metadata is captured
//!
//! Run with: cargo run --bin test_latency

use futures_util::StreamExt;
use neurovisor::ollama::{OllamaClient, StreamChunk};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("┌─────────────────────────────────────────┐");
    println!("│  Testing Streaming Latency Capture      │");
    println!("└─────────────────────────────────────────┘\n");

    let client = OllamaClient::new("http://localhost:11434");

    let prompt = "Say 'hello world' and nothing else.";
    let model = "llama3.2";

    println!("Prompt: {}", prompt);
    println!("Model:  {}\n", model);
    println!("─── Streaming Response ───");

    let mut stream = client.generate_stream(prompt, model, None).await?;

    let mut token_count = 0;

    // Process the stream
    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(StreamChunk::Token(token)) => {
                // Print tokens as they arrive (no newline for continuous output)
                print!("{}", token);
                token_count += 1;
            }
            Ok(StreamChunk::Done(metadata)) => {
                // Final chunk - print the metadata
                println!("\n\n─── Metadata from Final Chunk ───");
                println!("┌────────────────────────────────────────┐");
                println!("│ eval_count (tokens):    {:>14} │", metadata.eval_count);
                println!("│ prompt_eval_count:      {:>14} │", metadata.prompt_eval_count);
                println!("│ eval_duration_ns:       {:>14} │", metadata.eval_duration_ns);
                println!("│                                        │");

                // Convert to milliseconds for readability
                let latency_ms = (metadata.eval_duration_ns as f64) / 1_000_000.0;
                println!("│ Latency (calculated):   {:>11.2} ms │", latency_ms);
                println!("│ Tokens counted locally: {:>14} │", token_count);
                println!("└────────────────────────────────────────┘");

                // Verify latency is non-zero
                if metadata.eval_duration_ns > 0 {
                    println!("\n✅ SUCCESS: Latency metadata captured correctly!");
                } else {
                    println!("\n❌ FAILURE: Latency is zero (not captured)");
                }
            }
            Err(e) => {
                eprintln!("\n❌ Error: {}", e);
                return Err(e);
            }
        }
    }

    Ok(())
}
