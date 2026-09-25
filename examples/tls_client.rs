//! TLS client example
//!
//! Run with: `cargo run --example tls_client`
//! Connects to a TLS server (see tls_server.rs)
//
use wagonet::{
    ClientTLS,
    timeout_config::{KeepAliveConfig, TimeoutConfig},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // For production: use real hostname with valid certificate
    let mut client = ClientTLS::new("127.0.0.1:8443".to_string());

    // Important: set the domain for SNI and certificate verification
    client.set_domain("localhost".to_string());

    // For self-signed certificates in development ONLY:
    // client.set_accept_invalid_certs(true);

    // Enable keep-alive
    client.set_keep_alive(true).await;
    client.set_timeout_config(TimeoutConfig {
        keep_alive: Some(KeepAliveConfig {
            time: std::time::Duration::from_secs(60),
            interval: std::time::Duration::from_secs(15),
        }),
        ..Default::default()
    }).await;

    println!("Connecting with TLS...");
    client.connect().await?;
    println!("TLS connection established!");

    for i in 1..=3 {
        let msg = format!("TLS Message #{i}");
        let response = client.handle_message(1, msg.as_bytes()).await?;
        println!("Response: {:?}", String::from_utf8_lossy(&response));

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    client.disconnect().await?;
    println!("Disconnected.");

    Ok(())
}
