//! gRPC-like server using Prost for message serialization.
//!
//! Demonstrates:
//! - Using Prost-generated types with wagonet
//! - Multiple commands with typed messages
//! - Shared state across connections (AtomicU64)
//!
//! Run with: `cargo run --example prost_server`
//! Then run prost_client in another terminal.

use async_trait::async_trait;
use prost::Message;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use wagonet::protocol_structs::ResponseStatus;
use wagonet::{ProtocolCommandHandler, Result, Server};

// Import pre-generated Prost types (no protoc needed at build time)
#[path = "types.rs"]
mod greet;

const CMD_GREET: u32 = 1;
const CMD_STATS: u32 = 2;

struct GreeterHandler {
    total_requests: Arc<AtomicU64>,
    total_greetings: Arc<AtomicU64>,
}

#[async_trait]
impl ProtocolCommandHandler for GreeterHandler {
    async fn run(&mut self, command: u32, data: Vec<u8>) -> Result<(ResponseStatus, Vec<u8>)> {
        self.total_requests.fetch_add(1, Ordering::Relaxed);

        match command {
            CMD_GREET => {
                // Decode the Protobuf request
                let request = greet::GreetRequest::decode(data.as_slice())
                    .map_err(|e| format!("Failed to decode GreetRequest: {}", e))?;

                let times = if request.times == 0 { 1 } else { request.times };
                let mut greetings = Vec::with_capacity(times as usize);

                for i in 0..times {
                    greetings.push(format!("Hello, {}! (greeting #{})", request.name, i + 1));
                }

                let total = self
                    .total_greetings
                    .fetch_add(times as u64, Ordering::Relaxed)
                    + times as u64;

                let response = greet::GreetResponse {
                    greetings,
                    total_greetings: total,
                };

                // Encode the Protobuf response
                let mut buf = Vec::with_capacity(response.encoded_len());
                response
                    .encode(&mut buf)
                    .map_err(|e| format!("Failed to encode GreetResponse: {}", e))?;

                Ok((ResponseStatus::Ok, buf))
            }
            CMD_STATS => {
                // Decode the (empty) request for validation
                let _request = greet::StatsRequest::decode(data.as_slice())
                    .map_err(|e| format!("Failed to decode StatsRequest: {}", e))?;

                let response = greet::StatsResponse {
                    total_requests: self.total_requests.load(Ordering::Relaxed),
                    active_connections: 0, // Would need connection tracking
                };

                let mut buf = Vec::with_capacity(response.encoded_len());
                response
                    .encode(&mut buf)
                    .map_err(|e| format!("Failed to encode StatsResponse: {}", e))?;

                Ok((ResponseStatus::Ok, buf))
            }
            _ => Err(format!("Unknown command: {}", command).into()),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt::init();

    let total_requests = Arc::new(AtomicU64::new(0));
    let total_greetings = Arc::new(AtomicU64::new(0));

    let mut server = Server::new("127.0.0.1:9090".to_string(), 10 * 1024 * 1024);
    server.set_protocol_command_handler_factory({
        let req = total_requests.clone();
        let grt = total_greetings.clone();
        move || {
            Box::new(GreeterHandler {
                total_requests: req.clone(),
                total_greetings: grt.clone(),
            })
        }
    });

    let shutdown_handle = server.shutdown_handle();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        println!("\nShutdown signal received...");
        let _ = shutdown_handle.send(true);
    });

    println!("Prost server listening on 127.0.0.1:9090");
    println!("Commands: 1=greet (GreetRequest), 2=stats (StatsRequest)");
    println!("Press Ctrl+C to stop.");

    server.run().await
}
