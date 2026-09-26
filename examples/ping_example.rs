use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use wagonet::{ClientTL, TimeoutConfig, server_tl::ServerTL};

/// Ping example demonstrating application-level keep-alive.
///
/// This example shows how the ping protocol keeps a connection alive
/// during idle periods, preventing server-side read_header timeouts.
///
/// Run with: cargo run --example ping_example
#[tokio::main]
async fn main() -> wagonet::Result<()> {
    // Initialize tracing for debug output
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let server_ready = Arc::new(Notify::new());
    let server_ready_clone = server_ready.clone();

    // Connection counter to verify no reconnection occurs
    let connection_count = Arc::new(AtomicUsize::new(0));
    let connection_count_clone = connection_count.clone();

    // Server: read_header timeout = 500ms, but client pings every 100ms
    let server_handle = tokio::spawn(async move {
        server_ready_clone.notify_one();

        let (mut stream, _) = listener.accept().await.unwrap();
        connection_count_clone.fetch_add(1, Ordering::SeqCst);
        eprintln!(
            "[server] New connection accepted (total: {})",
            connection_count_clone.load(Ordering::SeqCst)
        );

        let (reader, writer) = stream.split();
        let mut server = ServerTL::new(reader, writer);

        // Short read_header timeout to demonstrate ping effectiveness
        server.set_timeout_config(TimeoutConfig {
            read_header: Duration::from_millis(500),
            ..Default::default()
        });

        while let Some((command, data_size)) = server.read_command().await.unwrap() {
            eprintln!("[server] Got command={}, data_size={}", command, data_size);

            let mut data = Vec::new();
            if data_size > 0 {
                data = server.receive_data(data_size).await.unwrap();
                eprintln!(
                    "[server] Received data: {:?}",
                    String::from_utf8_lossy(&data)
                );
            }

            let response = format!("Echo: {}", String::from_utf8_lossy(&data));
            server
                .send_data(1, Some(response.as_bytes()))
                .await
                .unwrap();
            eprintln!("[server] Sent response");
        }
        eprintln!("[server] Connection closed");
    });

    server_ready.notified().await;

    // Client: keep_alive=true, ping_interval=100ms (much less than server's 500ms)
    let mut client = ClientTL::new(addr.clone());
    client.set_keep_alive(true).await;
    client
        .set_timeout_config(TimeoutConfig {
            read_header: Duration::from_millis(500),
            ping_interval: Duration::from_millis(100),
            ..Default::default()
        })
        .await;

    eprintln!("[client] Sending first request...");
    let response1 = client.handle_message(42, b"Hello").await?;
    eprintln!(
        "[client] Response 1: {:?}",
        String::from_utf8_lossy(&response1)
    );

    // Idle for 2 seconds - without ping, server would timeout after 500ms
    // With ping every 100ms, connection stays alive
    eprintln!("[client] Idling for 2 seconds (ping should keep connection alive)...");
    tokio::time::sleep(Duration::from_secs(2)).await;

    eprintln!("[client] Sending second request after idle...");
    let response2 = client.handle_message(43, b"World").await?;
    eprintln!(
        "[client] Response 2: {:?}",
        String::from_utf8_lossy(&response2)
    );

    // Verify only one connection was made
    let final_count = connection_count.load(Ordering::SeqCst);
    eprintln!("[client] Total connections made: {}", final_count);

    assert_eq!(
        final_count, 1,
        "Expected exactly 1 connection (no reconnect during idle)"
    );
    assert_eq!(response1, b"Echo: Hello");
    assert_eq!(response2, b"Echo: World");

    // Clean shutdown
    client.disconnect().await?;
    server_handle.await.unwrap();

    eprintln!("[client] SUCCESS: Connection survived idle period thanks to ping!");
    Ok(())
}