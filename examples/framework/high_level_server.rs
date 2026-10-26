//! High-level echo server built on the `Server` framework.
//!
//! Demonstrates:
//! - Custom `ProtocolCommandHandler` with multiple commands
//! - Per-connection state (request counter)
//! - Returning errors, finishing connections
//!
//! Run with: `cargo run --example high_level_server`
//! Then run high_level_client in another terminal.

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use wagonet::protocol_structs::ResponseStatus;
use wagonet::{ProtocolCommandHandler, Result, Server};

/// Command IDs used by this example (command 0 is reserved for keep-alive pings).
const CMD_ECHO: u32 = 1;
const CMD_UPPERCASE: u32 = 2;
const CMD_FINISH: u32 = 3;
const CMD_STATS: u32 = 4;

struct EchoHandler {
    /// Per-connection request counter.
    requests: u64,
    /// Shared total counter across all connections.
    total_requests: Arc<AtomicU64>,
}

#[async_trait]
impl ProtocolCommandHandler for EchoHandler {
    async fn run(&mut self, command: u32, data: Vec<u8>) -> Result<(ResponseStatus, Vec<u8>)> {
        self.requests += 1;
        self.total_requests.fetch_add(1, Ordering::Relaxed);

        match command {
            CMD_ECHO => {
                // Echo the payload back as-is
                Ok((ResponseStatus::Ok, data))
            }
            CMD_UPPERCASE => {
                let upper = String::from_utf8_lossy(&data).to_uppercase().into_bytes();
                Ok((ResponseStatus::Ok, upper))
            }
            CMD_FINISH => {
                // Tell the framework to close the connection after this response
                Ok((ResponseStatus::Finish, b"Goodbye!".to_vec()))
            }
            CMD_STATS => {
                let msg = format!(
                    "This connection: {} requests | Total across all connections: {}",
                    self.requests,
                    self.total_requests.load(Ordering::Relaxed)
                );
                Ok((ResponseStatus::Ok, msg.into_bytes()))
            }
            _ => {
                // Unknown command — report application error, keep connection alive
                Err(format!("unknown command: {}", command).into())
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialize tracing for logs
    tracing_subscriber::fmt::init();

    let total_requests = Arc::new(AtomicU64::new(0));

    let mut server = Server::new("127.0.0.1:8080".to_string(), 10 * 1024 * 1024);
    server.set_protocol_command_handler_factory({
        let total = total_requests.clone();
        move || {
            Box::new(EchoHandler {
                requests: 0,
                total_requests: total.clone(),
            })
        }
    });

    // Ctrl+C triggers graceful shutdown
    let shutdown_handle = server.shutdown_handle();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        println!("\nShutdown signal received, stopping server...");
        let _ = shutdown_handle.send(true);
    });

    println!("Server listening on 127.0.0.1:8080");
    println!("Commands: 1=echo, 2=uppercase, 3=finish, 4=stats");
    println!("Press Ctrl+C to stop.");

    server.run().await
}
