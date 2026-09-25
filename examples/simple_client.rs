//! Simple TCP client example
//!
//! Run with: `cargo run --example simple_client`
//! Requires a server running on 127.0.0.1:8080 (see simple_server.rs)
//
use wagonet::{
    ClientTL,
    timeout_config::{KeepAliveConfig, TimeoutConfig},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("Connecting to 127.0.0.1:8080...");

    let mut client = ClientTL::new("127.0.0.1:8080".to_string());

    // Optional: enable keep-alive for connection reuse
    client.set_keep_alive(true).await;
    client
        .set_timeout_config(TimeoutConfig {
            keep_alive: Some(KeepAliveConfig {
                time: std::time::Duration::from_secs(60),
                interval: std::time::Duration::from_secs(15),
            }),
            ..Default::default()
        })
        .await;

    // Connect (optional - handle_message will auto-connect)
    client.connect().await?;
    println!("Connected!");

    // Send a few messages - connection will be reused
    for i in 1..=5 {
        let msg = format!("Message #{i}");
        let response = client.handle_message(1, msg.as_bytes()).await?;
        println!("Response: {:?}", String::from_utf8_lossy(&response));

        // Small delay between messages
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // Clean disconnect
    client.disconnect().await?;
    println!("Disconnected.");

    Ok(())
}
