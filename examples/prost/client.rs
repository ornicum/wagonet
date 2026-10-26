//! Client for the prost_server example.
//!
//! Demonstrates:
//! - Encoding Protobuf requests
//! - Decoding Protobuf responses
//! - Multiple requests over keep-alive connection
//!
//! Run with: `cargo run --example prost_client`
//! Requires prost_server running on 127.0.0.1:9090.

use prost::Message;
use wagonet::ClientTL;

// Import pre-generated Prost types (no protoc needed at build time)
#[path = "types.rs"]
mod greet;

const CMD_GREET: u32 = 1;
const CMD_STATS: u32 = 2;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("Connecting to 127.0.0.1:9090...");

    let mut client = ClientTL::new("127.0.0.1:9090".to_string());
    client.set_keep_alive(true).await;
    client.connect().await?;
    println!("Connected!\n");

    // Request 1: Greet Alice 3 times
    let request = greet::GreetRequest {
        name: "Alice".to_string(),
        times: 3,
    };
    let mut buf = Vec::with_capacity(request.encoded_len());
    request.encode(&mut buf)?;

    let response_bytes = client.handle_message(CMD_GREET, &buf).await?;
    let response = greet::GreetResponse::decode(response_bytes.as_slice())?;

    println!("Greet Alice (3 times):");
    for greeting in &response.greetings {
        println!("  - {}", greeting);
    }
    println!("  Total greetings so far: {}\n", response.total_greetings);

    // Request 2: Greet Bob once
    let request = greet::GreetRequest {
        name: "Bob".to_string(),
        times: 1,
    };
    let mut buf = Vec::with_capacity(request.encoded_len());
    request.encode(&mut buf)?;

    let response_bytes = client.handle_message(CMD_GREET, &buf).await?;
    let response = greet::GreetResponse::decode(response_bytes.as_slice())?;

    println!("Greet Bob (1 time):");
    for greeting in &response.greetings {
        println!("  - {}", greeting);
    }
    println!("  Total greetings so far: {}\n", response.total_greetings);

    // Request 3: Get stats
    let request = greet::StatsRequest {};
    let mut buf = Vec::with_capacity(request.encoded_len());
    request.encode(&mut buf)?;

    let response_bytes = client.handle_message(CMD_STATS, &buf).await?;
    let response = greet::StatsResponse::decode(response_bytes.as_slice())?;

    println!("Server stats:");
    println!("  Total requests: {}", response.total_requests);
    println!("  Active connections: {}", response.active_connections);

    client.disconnect().await?;
    println!("\nDisconnected.");
    Ok(())
}
