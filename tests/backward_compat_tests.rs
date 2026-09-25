#[cfg(test)]
#[path = "common.rs"]
mod common;
use common::spawn_server_with_shutdown;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use wagonet::{ClientTL, TimeoutConfig, server_tl::ServerTL};

/// Test backward compatibility: Old client (no ping) vs New server (with ping support)
/// The old client doesn't send pings, but the new server should handle regular requests normally
#[tokio::test]
async fn backward_compat_old_client_new_server() {
    let connection_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let connection_count_clone = connection_count.clone();

    let (server_handle, server_shutdown, addr) = spawn_server_with_shutdown(move |mut stream| {
        let connection_count = connection_count_clone.clone();
        async move {
            connection_count.fetch_add(1, Ordering::SeqCst);
            let (reader, writer) = stream.split();
            let mut server = ServerTL::new(reader, writer);
            server.set_timeout_config(TimeoutConfig {
                read_header: Duration::from_secs(10),
                ..Default::default()
            });

            // Handle multiple requests without ping
            for _ in 0..3 {
                let (command, data_size) = match server.read_command().await {
                    Ok((cmd, sz)) => (cmd, sz),
                    Err(_) => break,
                };

                if command == 0 && data_size == 0 {
                    continue; // Ping handled by read_command
                }

                if data_size > 0 {
                    let _ = server.receive_data(data_size).await;
                }
                let _ = server.send_data(1, Some(b"OK")).await;
            }
        }
    }).await;

    // Old client (no ping support, no keep_alive ping)
    let mut client = ClientTL::new(addr.to_string());
    client.set_keep_alive(true).await;
    client.set_timeout_config(TimeoutConfig {
        read_header: Duration::from_secs(10),
        ..Default::default()
    }).await;

    // Send 3 requests without ping
    for i in 1..=3 {
        let response = client.handle_message(i, b"data").await.unwrap();
        assert_eq!(response, b"OK");
    }

    // Only 1 connection should be made
    let final_count = connection_count.load(Ordering::SeqCst);
    assert_eq!(final_count, 1);

    client.disconnect().await.unwrap();
    server_shutdown.notify_one();
    server_handle.await.unwrap();
}

/// Test backward compatibility: New client (with ping) vs Old server (without ping support)
/// This test is complex due to protocol differences and is documented for future implementation.
/// The old server would treat ping (command=0) as a regular command with empty payload
/// and should respond with a valid ACK. Full backward compatibility requires
/// the old server to implement a compatible response protocol.
#[tokio::test]
#[ignore = "Complex protocol compatibility - requires old server to implement compatible response protocol"]
async fn backward_compat_new_client_old_server() {
    // TODO: Implement when old server protocol compatibility is resolved
    // The old server should treat ping (command=0, data_size=0) as a regular command
    // and respond with a valid ACK. The new client's ping response reader should
    // handle both 1-byte (new server) and multi-byte (old server) responses.
    panic!("Not implemented - complex protocol compatibility");
}