//! No-answer mode example (fire-and-forget)
//!
//! Run with: `cargo run --example no_answer_mode`
//! Requires a server that handles no-answer commands (status=1 in response)
//
use wagonet::{
    ClientTL,
    timeout_config::{KeepAliveConfig, TimeoutConfig},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("Connecting with no-answer mode...");

    let mut client = ClientTL::new("127.0.0.1:8080".to_string());

    // Enable no-answer mode: client won't wait for response
    client.set_command_has_answer(false);

    // Keep-alive still works
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

    client.connect().await?;
    println!("Connected in no-answer mode!");

    // Fire-and-forget messages
    for i in 1..=10 {
        let msg = format!("Event #{i}");
        // Returns empty Vec (no response expected)
        let _ = client.handle_message(42, msg.as_bytes()).await?;
        println!("Sent: {}", msg);

        // High throughput possible with no-answer mode
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    client.disconnect().await?;
    println!("Disconnected.");

    Ok(())
}
