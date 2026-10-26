//! Client for the high_level_server example.
//!
//! Demonstrates:
//! - Multiple requests over a single keep-alive connection
//! - Different command IDs
//! - Error handling (unknown command)
//! - Connection termination via CMD_FINISH
//!
//! Run with: `cargo run --example high_level_client`
//! Requires high_level_server running on 127.0.0.1:8080.

use wagonet::ClientTL;

const CMD_ECHO: u32 = 1;
const CMD_UPPERCASE: u32 = 2;
const CMD_STATS: u32 = 4;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("Connecting to 127.0.0.1:8080...");

    let mut client = ClientTL::new("127.0.0.1:8080".to_string());
    client.set_keep_alive(true).await;
    client.connect().await?;
    println!("Connected!\n");

    // Echo
    let response = client.handle_message(CMD_ECHO, b"Hello, wagonet!").await?;
    println!("Echo:     {:?}", String::from_utf8_lossy(&response));

    // Uppercase
    let response = client.handle_message(CMD_UPPERCASE, b"shout it").await?;
    println!("Upper:    {:?}", String::from_utf8_lossy(&response));

    // Stats — same connection, so server counts our requests
    let response = client.handle_message(CMD_STATS, b"").await?;
    println!("Stats:    {:?}", String::from_utf8_lossy(&response));

    // Unknown command — server returns Error status, connection stays alive
    match client.handle_message(999, b"nope").await {
        Ok(_) => println!("Unexpected success for unknown command"),
        Err(wagonet::Error::ResponseError { status }) => {
            println!("Expected error for unknown cmd: status={}", status);
        }
        Err(e) => println!("Unexpected error kind: {:?}", e),
    }

    // Another request still works — the connection survived the error
    let response = client.handle_message(CMD_ECHO, b"still alive").await?;
    println!("After err: {:?}", String::from_utf8_lossy(&response));

    // Send a request to a second server instance by opening a fresh connection
    // (disable keep-alive so this request uses a new connection)
    client.set_keep_alive(false).await;
    let response = client.handle_message(CMD_STATS, b"").await?;
    println!("New conn:  {:?}", String::from_utf8_lossy(&response));

    client.disconnect().await?;
    println!("\nDisconnected.");
    Ok(())
}
