//! Graceful shutdown example.
//!
//! Demonstrates:
//! - Using `Server::shutdown()` to stop the accept loop
//! - Using `shutdown_handle()` to shut down from another task
//! - Active connections finish their current request before being dropped
//!
//! Run with: `cargo run --example shutdown_example`
//! The server shuts down automatically after 3 seconds.

use async_trait::async_trait;
use std::time::Duration;
use wagonet::protocol_structs::ResponseStatus;
use wagonet::{ProtocolCommandHandler, Result, Server};

struct SlowHandler;

#[async_trait]
impl ProtocolCommandHandler for SlowHandler {
    async fn run(&mut self, command: u32, data: Vec<u8>) -> Result<(ResponseStatus, Vec<u8>)> {
        // Simulate work — a request in flight when shutdown is signaled will
        // still complete before the connection task exits.
        tokio::time::sleep(Duration::from_millis(200)).await;
        println!(
            "  [handler] cmd={}, payload={:?}",
            command,
            String::from_utf8_lossy(&data)
        );
        Ok((ResponseStatus::Ok, data))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut server = Server::new("127.0.0.1:8081".to_string(), 1024 * 1024);
    server.set_protocol_command_handler_factory(|| Box::new(SlowHandler));

    // Get a clone of the shutdown sender BEFORE run() starts
    let shutdown_handle = server.shutdown_handle();

    // Start server in a background task
    let server_handle = tokio::spawn(async move { server.run().await });

    println!("Server started on 127.0.0.1:8081");

    // Spawn a client that sends requests in a loop
    let client_handle = tokio::spawn(async {
        let mut client = wagonet::ClientTL::new("127.0.0.1:8081".to_string());
        client.set_keep_alive(true).await;

        for i in 0..10 {
            match client
                .handle_message(1, format!("msg {}", i).as_bytes())
                .await
            {
                Ok(r) => println!("[client] got: {:?}", String::from_utf8_lossy(&r)),
                Err(e) => {
                    println!("[client] error (expected after shutdown): {:?}", e);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });

    // Schedule shutdown after 3 seconds
    tokio::time::sleep(Duration::from_secs(3)).await;
    println!("\n>>> Triggering shutdown...");
    let _ = shutdown_handle.send(true);

    // Wait for both tasks to finish
    let _ = server_handle.await;
    println!("Server stopped.");
    let _ = client_handle.await;
    println!("Client stopped.");

    Ok(())
}
