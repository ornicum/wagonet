//! Keep-alive tests using the common test server infrastructure.

#[cfg(test)]
#[path = "common.rs"]
mod common;
use common::spawn_server_with_shutdown;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio::sync::Notify;
use wagonet::{ClientTL, TimeoutConfig};
/// Spawns a mock server implementing the wagonet binary protocol with graceful shutdown.
///
/// Returns (server_handle, shutdown_signal, server_addr).
///
/// The server implements:
/// - 8-byte request header (command + data_size, big-endian)
/// - 1-byte ACK header (status=1)
/// - Payload read (if data_size > 0)
/// - 5-byte response header (status + data_size, big-endian)
/// - Response payload
/// - 1-byte response ACK
///
/// - `request_counter`: optional atomic counter incremented on every new TCP connection.
/// - `fail_on_first`: if true, the server drops the connection immediately after reading
///   the request header of the FIRST request on a given connection (used to simulate
///   a transient network failure mid-exchange).
async fn spawn_mock_server(
    request_counter: Option<Arc<AtomicUsize>>,
    fail_on_first: bool,
) -> (JoinHandle<()>, Arc<Notify>, std::net::SocketAddr) {
    use std::sync::atomic::AtomicBool;
    // Track if we've already failed on the first connection
    let first_connection_failed = Arc::new(AtomicBool::new(false));
    let first_connection_failed_clone = first_connection_failed.clone();

    spawn_server_with_shutdown(move |mut stream: TcpStream| {
        let request_counter = request_counter.clone();
        let fail_on_first = fail_on_first;
        let first_connection_failed = first_connection_failed_clone.clone();
        async move {
            let mut req_count = 0;

            if let Some(counter) = &request_counter {
                counter.fetch_add(1, Ordering::SeqCst);
            }

            // Handle multiple requests on the same TCP connection (keep-alive loop)
            loop {
                req_count += 1;

                // STEP 1: Read the Request Header (8 bytes: 4 bytes command + 4 bytes data_size, Big-Endian)
                let mut req_header = [0u8; 8];
                if stream.read_exact(&mut req_header).await.is_err() {
                    break; // Client closed the connection
                }

                let _command = u32::from_be_bytes([
                    req_header[0],
                    req_header[1],
                    req_header[2],
                    req_header[3],
                ]);
                let data_size = u32::from_be_bytes([
                    req_header[4],
                    req_header[5],
                    req_header[6],
                    req_header[7],
                ]) as usize;

                // Simulate a failure: drop the connection right after receiving the
                // first request's header, before sending any response.
                // Only fail on the very first connection's first request.
                let should_fail = fail_on_first
                    && req_count == 1
                    && !first_connection_failed.swap(true, Ordering::SeqCst);
                if should_fail {
                    drop(stream);
                    break;
                }

                // STEP 2: Send the ACK Header (1 byte, status = 1 meaning OK)
                if stream.write_all(&[1u8]).await.is_err() {
                    break;
                }

                // STEP 3: Read the payload (if any)
                if data_size > 0 {
                    let mut payload = vec![0u8; data_size];
                    if stream.read_exact(&mut payload).await.is_err() {
                        break;
                    }
                }

                // STEP 4: Send the Response Header (5 bytes: 1 byte status + 4 bytes data_size)
                let response_data = b"OK";
                let response_header = [1u8, 0, 0, 0, response_data.len() as u8];
                if stream.write_all(&response_header).await.is_err() {
                    break;
                }

                // STEP 5: Send the response payload
                if stream.write_all(response_data).await.is_err() {
                    break;
                }

                // STEP 6: Read the response ACK (1 byte)
                let mut ack = [0u8; 1];
                if stream.read_exact(&mut ack).await.is_err() {
                    break;
                }
            }
        }
    }).await
}

/// Test 1: Happy path — verify that keep-alive actually reuses the same TCP connection
/// across multiple sequential `handle_message` calls.
#[tokio::test]
async fn test_keep_alive_reuses_connection() {
    let connection_count = Arc::new(AtomicUsize::new(0));
    let (_server_handle, server_shutdown, addr) = spawn_mock_server(Some(connection_count.clone()), false).await;

    let mut client = ClientTL::new(addr.to_string());
    client.set_keep_alive(true).await;

    // First request — establishes a new TCP connection.
    let _ = client.handle_message(1, b"hello").await.unwrap();
    // Second request — must reuse the existing connection.
    let _ = client.handle_message(2, b"world").await.unwrap();

    // We expect exactly 1 TCP connection to have been accepted by the server.
    assert_eq!(
        connection_count.load(Ordering::SeqCst),
        1,
        "Expected exactly 1 TCP connection to be reused across requests"
    );

    server_shutdown.notify_one();
}

/// Test 2: Critical regression test — verify that the automatic retry on transient
/// failure does NOT cause the same request to be sent twice.
///
/// This test documents the bug we fixed: previously, `handle_message` would silently
/// re-invoke `try_send_receive` after a read failure, which caused duplicate
/// non-idempotent requests on the server side.
#[tokio::test]
async fn test_keep_alive_no_duplicate_on_failure() {
    let connection_count = Arc::new(AtomicUsize::new(0));
    let (_server_handle, server_shutdown, addr) = spawn_mock_server(Some(connection_count.clone()), true).await;

    let mut client = ClientTL::new(addr.to_string());
    client.set_keep_alive(true).await;
    client.set_timeout_config(TimeoutConfig {
        read_header: Duration::from_millis(500),
        ..Default::default()
    }).await;

    // First call fails because the server drops the connection.
    let res1 = client.handle_message(1, b"test").await;
    assert!(
        res1.is_err(),
        "First call should fail due to the simulated server-side drop"
    );

    // Second call must attempt a fresh connection (the previous one was cleared).
    // It will also fail for the same reason, but the important thing is that the
    // client does not panic and does try to reconnect.
    let _res2 = client.handle_message(2, b"test2").await;

    // Two separate TCP connection attempts should have been made — one per call.
    assert_eq!(
        connection_count.load(Ordering::SeqCst),
        2,
        "Expected 2 separate TCP connection attempts, one per handle_message call"
    );

    server_shutdown.notify_one();
}

/// Test 3: Verify that after a failed request the client correctly clears its
/// internal stream state and is able to establish a fresh connection on the next call.
#[tokio::test]
async fn test_keep_alive_recovers_on_next_call() {
    let connection_count = Arc::new(AtomicUsize::new(0));
    let (_server_handle, server_shutdown, addr) = spawn_mock_server(Some(connection_count.clone()), true).await;

    let mut client = ClientTL::new(addr.to_string());
    client.set_keep_alive(true).await;
    client.set_timeout_config(TimeoutConfig {
        read_header: Duration::from_millis(500),
        ..Default::default()
    }).await;

    // First call fails because the server drops the connection.
    let res1 = client.handle_message(1, b"test").await;
    assert!(res1.is_err(), "First call should fail");

    // Give the client a moment to clear its internal state
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second call should establish a fresh connection and succeed
    // (the mock server doesn't fail on the second connection)
    let res2 = client.handle_message(2, b"test2").await;
    assert!(res2.is_ok(), "Second call should succeed after reconnect");

    // Two connections: first one failed, second one succeeded
    assert_eq!(
        connection_count.load(Ordering::SeqCst),
        2,
        "Expected 2 connections: 1 failed, 1 successful reconnect"
    );

    server_shutdown.notify_one();
}