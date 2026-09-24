#[cfg(test)]
mod keep_alive_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use wagonet::{ClientTL, TimeoutConfig};

    /// Helper: a mock server that implements the wagonet binary protocol
    /// (8-byte request header, 1-byte ACK, 5-byte response header, 1-byte response ACK).
    ///
    /// - `request_counter`: optional atomic counter incremented on every new TCP connection.
    /// - `fail_on_first`: if true, the server drops the connection immediately after reading
    ///   the request header of the FIRST request on a given connection (used to simulate
    ///   a transient network failure mid-exchange).
    async fn run_mock_server(
        listener: TcpListener,
        request_counter: Option<Arc<AtomicUsize>>,
        fail_on_first: bool,
    ) {
        tokio::spawn(async move {
            let mut req_count = 0;
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
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
                    if fail_on_first && req_count == 1 {
                        drop(stream);
                        break;
                    }

                    // STEP 2: Send the ACK Header (1 byte, status = 1 meaning OK)
                    stream.write_all(&[1u8]).await.unwrap();

                    // STEP 3: Read the Request Data payload
                    if data_size > 0 {
                        let mut req_data = vec![0u8; data_size];
                        if stream.read_exact(&mut req_data).await.is_err() {
                            break;
                        }
                    }

                    // STEP 4: Send the Response Header (5 bytes: status + data_size, Big-Endian)
                    // Here we send status=1 and data_size=0 (empty payload).
                    stream.write_all(&[1u8, 0, 0, 0, 0]).await.unwrap();

                    // STEP 5: Read the Response ACK from the client.
                    // wagonet's `send_response_header(1, 0)` always sends 1 byte because
                    // `is_default = true` when data_size == 0.
                    let mut resp_ack = [0u8; 1];
                    if stream.read_exact(&mut resp_ack).await.is_err() {
                        break;
                    }

                    // STEP 6: Send the Response Data (0 bytes in this mock, since data_size == 0).
                    // The loop continues to handle the next request over the same keep-alive connection.
                }
            }
        });
    }

    /// Test 1: Happy path — verify that keep-alive actually reuses the same TCP connection
    /// across multiple sequential `handle_message` calls.
    #[tokio::test]
    async fn test_keep_alive_reuses_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connection_count = Arc::new(AtomicUsize::new(0));

        run_mock_server(listener, Some(connection_count.clone()), false).await;

        let mut client = ClientTL::new(addr.to_string());
        client.set_keep_alive(true);

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
    }

    /// Test 2: Critical regression test — verify that the automatic retry on transient
    /// failure does NOT cause the same request to be sent twice.
    ///
    /// This test documents the bug we fixed: previously, `handle_message` would silently
    /// re-invoke `try_send_receive` after a read failure, which caused duplicate
    /// non-idempotent requests on the server side.
    #[tokio::test]
    async fn test_keep_alive_no_duplicate_on_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let request_received_count = Arc::new(AtomicUsize::new(0));

        // The server intentionally drops the connection after reading the first request's
        // header, simulating a transient network failure mid-exchange.
        run_mock_server(listener, Some(request_received_count.clone()), true).await;

        let mut client = ClientTL::new(addr.to_string());
        client.set_keep_alive(true);
        client.set_timeout_config(TimeoutConfig {
            read_header: Duration::from_millis(500),
            ..Default::default()
        });

        // The call must return an error because the server drops the connection.
        let result = client.handle_message(99, b"test_data").await;
        assert!(
            result.is_err(),
            "Expected an error due to the server dropping the connection"
        );

        // KEY ASSERTION:
        // After the fix, the request must have been sent EXACTLY ONCE.
        // If the buggy automatic retry were still present, the counter would be 2.
        assert_eq!(
            request_received_count.load(Ordering::SeqCst),
            1,
            "BUG REGRESSION: The request was sent more than once! \
             The automatic retry in `handle_message` must be removed."
        );
    }

    /// Test 3: Verify that after a failed request the client correctly clears its
    /// internal stream state and is able to establish a fresh connection on the next call.
    #[tokio::test]
    async fn test_keep_alive_recovers_on_next_call() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connection_count = Arc::new(AtomicUsize::new(0));

        // The server drops the connection on the first request of every new TCP connection,
        // so each `handle_message` call will result in a new connection attempt.
        run_mock_server(listener, Some(connection_count.clone()), true).await;

        let mut client = ClientTL::new(addr.to_string());
        client.set_keep_alive(true);
        client.set_timeout_config(TimeoutConfig {
            read_header: Duration::from_millis(500),
            ..Default::default()
        });

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
    }
}
