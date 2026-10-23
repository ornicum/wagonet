//! Integration tests for the high-level server framework:
//! `Server` + `ConnectionHandler` + `ProtocolCommandHandler`.
//!
//! Note: command 0 is reserved by the protocol for keep-alive pings,
//! so all tests use command ids >= 1.

use async_trait::async_trait;
use std::net::TcpListener as StdTcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::time::{sleep, timeout};
use wagonet::protocol_structs::ResponseStatus;
use wagonet::{ClientTL, Error, ProtocolCommandHandler, Result, Server};

// ============================================================================
// Test handlers
// ============================================================================

/// Echo handler: returns `command (4 bytes LE) + request payload` back.
struct EchoHandler {
    call_count: Arc<AtomicUsize>,
}

#[async_trait]
impl ProtocolCommandHandler for EchoHandler {
    async fn run(&mut self, command: u32, data: Vec<u8>) -> Result<(ResponseStatus, Vec<u8>)> {
        self.call_count.fetch_add(1, Ordering::SeqCst);

        let mut response = Vec::with_capacity(4 + data.len());
        response.extend_from_slice(&command.to_le_bytes());
        response.extend_from_slice(&data);

        Ok((ResponseStatus::Ok, response))
    }
}

/// Handler that always returns an application-level error.
struct ErrorHandler;

#[async_trait]
impl ProtocolCommandHandler for ErrorHandler {
    async fn run(&mut self, _command: u32, _data: Vec<u8>) -> Result<(ResponseStatus, Vec<u8>)> {
        Err(Error::Msg("Intentional error for testing".to_string()))
    }
}

/// Handler that asks the framework to close the connection after responding.
struct FinishHandler;

#[async_trait]
impl ProtocolCommandHandler for FinishHandler {
    async fn run(&mut self, _command: u32, _data: Vec<u8>) -> Result<(ResponseStatus, Vec<u8>)> {
        Ok((ResponseStatus::Finish, Vec::new()))
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn find_available_port() -> u16 {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("Failed to bind for port discovery");
    listener
        .local_addr()
        .expect("Failed to get local addr")
        .port()
}

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

// ============================================================================
// Tests
// ============================================================================

/// A single request-response cycle through the framework.
#[tokio::test]
async fn test_framework_echo_handler() -> Result<()> {
    let port = find_available_port();
    let addr = format!("127.0.0.1:{}", port);
    let call_count = Arc::new(AtomicUsize::new(0));
    let counter = call_count.clone();

    let mut server = Server::new(addr.clone(), 1024 * 1024);
    server.set_protocol_command_handler_factory(move || {
        Box::new(EchoHandler {
            call_count: counter.clone(),
        })
    });

    let server_handle = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(50)).await;

    let mut client = ClientTL::new(addr);
    let request_data = b"test data";
    let response = timeout(REQUEST_TIMEOUT, client.handle_message(42, request_data))
        .await
        .expect("request timed out")?;

    assert_eq!(response.len(), 4 + request_data.len());
    let cmd = u32::from_le_bytes(response[0..4].try_into().unwrap());
    assert_eq!(cmd, 42);
    assert_eq!(&response[4..], request_data);
    assert_eq!(call_count.load(Ordering::SeqCst), 1);

    server_handle.abort();
    Ok(())
}

/// Multiple sequential requests over one keep-alive connection.
#[tokio::test]
async fn test_framework_multiple_requests_same_connection() -> Result<()> {
    let port = find_available_port();
    let addr = format!("127.0.0.1:{}", port);
    let call_count = Arc::new(AtomicUsize::new(0));
    let counter = call_count.clone();

    let mut server = Server::new(addr.clone(), 1024 * 1024);
    server.set_protocol_command_handler_factory(move || {
        Box::new(EchoHandler {
            call_count: counter.clone(),
        })
    });

    let server_handle = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(50)).await;

    let mut client = ClientTL::new(addr);
    client.set_keep_alive(true).await;

    // Command ids 1..=3 (command 0 is the reserved Ping command)
    for i in 1u32..=3 {
        let data = format!("request-{}", i);
        let response = timeout(REQUEST_TIMEOUT, client.handle_message(i, data.as_bytes()))
            .await
            .expect("request timed out")?;

        let cmd = u32::from_le_bytes(response[0..4].try_into().unwrap());
        assert_eq!(cmd, i);
        assert_eq!(&response[4..], data.as_bytes());
    }

    assert_eq!(call_count.load(Ordering::SeqCst), 3);

    server_handle.abort();
    Ok(())
}

/// Multiple concurrent clients are served independently.
#[tokio::test]
async fn test_framework_multiple_clients() -> Result<()> {
    let port = find_available_port();
    let addr = format!("127.0.0.1:{}", port);
    let call_count = Arc::new(AtomicUsize::new(0));
    let counter = call_count.clone();

    let mut server = Server::new(addr.clone(), 1024 * 1024);
    server.set_protocol_command_handler_factory(move || {
        Box::new(EchoHandler {
            call_count: counter.clone(),
        })
    });

    let server_handle = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(50)).await;

    let mut handles = Vec::new();
    for i in 0..3 {
        let addr_clone = addr.clone();
        handles.push(tokio::spawn(async move {
            let mut client = ClientTL::new(addr_clone);
            let data = format!("client-{}", i);
            // Command ids 1..=3 (command 0 is the reserved Ping command)
            client.handle_message(i + 1, data.as_bytes()).await
        }));
    }

    for handle in handles {
        let joined = timeout(REQUEST_TIMEOUT, handle)
            .await
            .expect("client task timed out")
            .expect("client task panicked");
        let response = joined.expect("client request failed");
        assert!(response.len() >= 4);
    }

    assert_eq!(call_count.load(Ordering::SeqCst), 3);

    server_handle.abort();
    Ok(())
}

/// A command handler error is delivered to the client as an error status
/// and does NOT kill the connection.
#[tokio::test]
async fn test_framework_error_response() -> Result<()> {
    let port = find_available_port();
    let addr = format!("127.0.0.1:{}", port);

    let mut server = Server::new(addr.clone(), 1024 * 1024);
    server.set_protocol_command_handler_factory(|| Box::new(ErrorHandler));

    let server_handle = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(50)).await;

    let mut client = ClientTL::new(addr.clone());
    let result = timeout(REQUEST_TIMEOUT, client.handle_message(1, b"test"))
        .await
        .expect("request timed out");

    match result {
        Err(Error::ResponseError { status }) => {
            assert_eq!(status, ResponseStatus::Error as u8);
        }
        other => panic!("Expected ResponseError, got: {:?}", other),
    }

    // The connection survived the application error: a second request
    // also receives an error response instead of EOF.
    let result2 = timeout(REQUEST_TIMEOUT, client.handle_message(2, b"test2"))
        .await
        .expect("second request timed out");
    assert!(matches!(result2, Err(Error::ResponseError { .. })));

    server_handle.abort();
    Ok(())
}

/// `ResponseStatus::Finish` closes the connection after the response.
#[tokio::test]
async fn test_framework_finish_status_closes_connection() -> Result<()> {
    let port = find_available_port();
    let addr = format!("127.0.0.1:{}", port);

    let mut server = Server::new(addr.clone(), 1024 * 1024);
    server.set_protocol_command_handler_factory(|| Box::new(FinishHandler));

    let server_handle = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(50)).await;

    let mut client = ClientTL::new(addr);
    let result = timeout(REQUEST_TIMEOUT, client.handle_message(1, b"test"))
        .await
        .expect("request timed out");

    match result {
        Err(Error::ResponseError { status }) => {
            assert_eq!(status, ResponseStatus::Finish as u8);
        }
        other => panic!("Expected Finish status, got: {:?}", other),
    }

    server_handle.abort();
    Ok(())
}

/// Server::shutdown() stops the accept loop gracefully.
#[tokio::test]
async fn test_framework_graceful_shutdown() -> Result<()> {
    let port = find_available_port();
    let addr = format!("127.0.0.1:{}", port);
    let call_count = Arc::new(AtomicUsize::new(0));
    let counter = call_count.clone();

    let mut server = Server::new(addr.clone(), 1024 * 1024);
    server.set_protocol_command_handler_factory(move || {
        Box::new(EchoHandler {
            call_count: counter.clone(),
        })
    });

    let shutdown_handle = server.shutdown_handle();
    let server_handle = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(50)).await;

    // The server works before shutdown
    let mut client = ClientTL::new(addr);
    let response = timeout(REQUEST_TIMEOUT, client.handle_message(1, b"before"))
        .await
        .expect("request timed out")?;
    assert!(response.len() >= 4);

    shutdown_handle
        .send(true)
        .expect("Failed to send shutdown signal");

    let stopped = timeout(Duration::from_secs(2), server_handle).await;
    assert!(stopped.is_ok(), "Server should shut down within 2 seconds");

    Ok(())
}

/// Server::run() returns an error when no command handler factory is set.
#[tokio::test]
async fn test_framework_server_without_handler() -> Result<()> {
    let mut server = Server::new("127.0.0.1:0".to_string(), 1024 * 1024);

    let result = timeout(Duration::from_secs(2), server.run())
        .await
        .expect("Server::run should not hang");
    assert!(
        result.is_err(),
        "Server must fail without a handler factory"
    );

    Ok(())
}

/// Handler that returns different statuses based on command id.
struct MultiStatusHandler;

#[async_trait]
impl ProtocolCommandHandler for MultiStatusHandler {
    async fn run(&mut self, command: u32, _data: Vec<u8>) -> Result<(ResponseStatus, Vec<u8>)> {
        match command {
            1 => Ok((ResponseStatus::Ok, b"ok".to_vec())),
            2 => Ok((ResponseStatus::InvalidCommand, Vec::new())),
            3 => Ok((ResponseStatus::InvalidData, Vec::new())),
            4 => Ok((ResponseStatus::Error, b"error msg".to_vec())),
            _ => Err(Error::Msg("unknown command".to_string())),
        }
    }
}

/// Tests that different ResponseStatus values are correctly delivered to client.
#[tokio::test]
async fn test_framework_multiple_response_statuses() -> Result<()> {
    let port = find_available_port();
    let addr = format!("127.0.0.1:{}", port);

    let mut server = Server::new(addr.clone(), 1024 * 1024);
    server.set_protocol_command_handler_factory(|| Box::new(MultiStatusHandler));

    let server_handle = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(50)).await;

    // Command 1: Ok status
    let mut client = ClientTL::new(addr.clone());
    client.set_keep_alive(true).await;

    let response = timeout(REQUEST_TIMEOUT, client.handle_message(1, b"test"))
        .await
        .expect("request timed out")?;
    assert_eq!(&response, b"ok");

    // Command 2: InvalidCommand status
    let result = timeout(REQUEST_TIMEOUT, client.handle_message(2, b"test"))
        .await
        .expect("request timed out");
    match result {
        Err(Error::ResponseError { status }) => {
            assert_eq!(status, ResponseStatus::InvalidCommand as u8);
        }
        other => panic!("Expected InvalidCommand, got: {:?}", other),
    }

    // Command 3: InvalidData status
    let result = timeout(REQUEST_TIMEOUT, client.handle_message(3, b"test"))
        .await
        .expect("request timed out");
    match result {
        Err(Error::ResponseError { status }) => {
            assert_eq!(status, ResponseStatus::InvalidData as u8);
        }
        other => panic!("Expected InvalidData, got: {:?}", other),
    }

    // Command 4: Error status with payload
    let result = timeout(REQUEST_TIMEOUT, client.handle_message(4, b"test"))
        .await
        .expect("request timed out");
    match result {
        Err(Error::ResponseError { status }) => {
            assert_eq!(status, ResponseStatus::Error as u8);
        }
        other => panic!("Expected Error, got: {:?}", other),
    }

    // Command 5: Handler error (becomes Error status)
    let result = timeout(REQUEST_TIMEOUT, client.handle_message(5, b"test"))
        .await
        .expect("request timed out");
    match result {
        Err(Error::ResponseError { status }) => {
            assert_eq!(status, ResponseStatus::Error as u8);
        }
        other => panic!("Expected Error, got: {:?}", other),
    }

    server_handle.abort();
    Ok(())
}

/// Handler that counts requests and returns the count.
struct CounterHandler {
    count: usize,
}

#[async_trait]
impl ProtocolCommandHandler for CounterHandler {
    async fn run(&mut self, _command: u32, _data: Vec<u8>) -> Result<(ResponseStatus, Vec<u8>)> {
        self.count += 1;
        Ok((ResponseStatus::Ok, self.count.to_le_bytes().to_vec()))
    }
}

/// Tests that handler state persists across multiple requests on same connection.
#[tokio::test]
async fn test_framework_handler_state_persistence() -> Result<()> {
    let port = find_available_port();
    let addr = format!("127.0.0.1:{}", port);

    let mut server = Server::new(addr.clone(), 1024 * 1024);
    server.set_protocol_command_handler_factory(|| Box::new(CounterHandler { count: 0 }));

    let server_handle = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(50)).await;

    let mut client = ClientTL::new(addr);
    client.set_keep_alive(true).await;

    // Send 10 requests and verify counter increments
    for expected_count in 1..=10 {
        let response = timeout(REQUEST_TIMEOUT, client.handle_message(1, b"test"))
            .await
            .expect("request timed out")?;

        let count = usize::from_le_bytes(response[0..8].try_into().unwrap());
        assert_eq!(count, expected_count);
    }

    server_handle.abort();
    Ok(())
}

/// Tests empty payload handling.
#[tokio::test]
async fn test_framework_empty_payload() -> Result<()> {
    let port = find_available_port();
    let addr = format!("127.0.0.1:{}", port);

    let mut server = Server::new(addr.clone(), 1024 * 1024);
    server.set_protocol_command_handler_factory(|| {
        Box::new(EchoHandler {
            call_count: Arc::new(AtomicUsize::new(0)),
        })
    });

    let server_handle = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(50)).await;

    let mut client = ClientTL::new(addr);
    let response = timeout(REQUEST_TIMEOUT, client.handle_message(42, &[]))
        .await
        .expect("request timed out")?;

    // Echo handler returns command (4 bytes) + empty data
    assert_eq!(response.len(), 4);
    let cmd = u32::from_le_bytes(response[0..4].try_into().unwrap());
    assert_eq!(cmd, 42);

    server_handle.abort();
    Ok(())
}

/// Tests large payload (near buffer limit).
#[tokio::test]
async fn test_framework_large_payload() -> Result<()> {
    let port = find_available_port();
    let addr = format!("127.0.0.1:{}", port);

    let mut server = Server::new(addr.clone(), 1024 * 1024);
    server.set_protocol_command_handler_factory(|| {
        Box::new(EchoHandler {
            call_count: Arc::new(AtomicUsize::new(0)),
        })
    });

    let server_handle = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(50)).await;

    let mut client = ClientTL::new(addr);

    // Create ~500KB payload (half of 1MB limit)
    let large_data: Vec<u8> = (0..500_000).map(|i| (i % 256) as u8).collect();

    let response = timeout(
        Duration::from_secs(10),
        client.handle_message(42, &large_data),
    )
    .await
    .expect("request timed out")?;

    assert_eq!(response.len(), 4 + large_data.len());
    assert_eq!(&response[4..], &large_data[..]);

    server_handle.abort();
    Ok(())
}

/// Tests rapid sequential requests without delays.
#[tokio::test]
async fn test_framework_rapid_sequential_requests() -> Result<()> {
    let port = find_available_port();
    let addr = format!("127.0.0.1:{}", port);
    let call_count = Arc::new(AtomicUsize::new(0));
    let counter = call_count.clone();

    let mut server = Server::new(addr.clone(), 1024 * 1024);
    server.set_protocol_command_handler_factory(move || {
        Box::new(EchoHandler {
            call_count: counter.clone(),
        })
    });

    let server_handle = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(50)).await;

    let mut client = ClientTL::new(addr);
    client.set_keep_alive(true).await;

    // Send 20 requests as fast as possible
    for i in 1..=20 {
        let data = format!("req-{}", i);
        let response = timeout(REQUEST_TIMEOUT, client.handle_message(i, data.as_bytes()))
            .await
            .expect("request timed out")?;

        let cmd = u32::from_le_bytes(response[0..4].try_into().unwrap());
        assert_eq!(cmd, i);
        assert_eq!(&response[4..], data.as_bytes());
    }

    assert_eq!(call_count.load(Ordering::SeqCst), 20);

    server_handle.abort();
    Ok(())
}

/// Tests that multiple independent connections each get their own handler instance.
/// Without keep-alive, each request creates a new handler (expected behavior).
#[tokio::test]
async fn test_framework_connection_isolation() -> Result<()> {
    let port = find_available_port();
    let addr = format!("127.0.0.1:{}", port);

    let mut server = Server::new(addr.clone(), 1024 * 1024);
    server.set_protocol_command_handler_factory(|| Box::new(CounterHandler { count: 0 }));

    let server_handle = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(50)).await;

    // Create 3 independent clients (no keep-alive = new connection per request)
    let mut handles = Vec::new();
    for _ in 0..3 {
        let addr_clone = addr.clone();
        handles.push(tokio::spawn(async move {
            let mut client = ClientTL::new(addr_clone);
            // Each request gets a NEW handler instance, so count is always 1
            for _ in 0..5 {
                let response = timeout(REQUEST_TIMEOUT, client.handle_message(1, b"test"))
                    .await
                    .expect("request timed out")?;
                let count = usize::from_le_bytes(response[0..8].try_into().unwrap());
                assert_eq!(
                    count, 1,
                    "Without keep-alive, each request gets a fresh handler"
                );
            }
            Ok::<_, Error>(())
        }));
    }

    for handle in handles {
        timeout(REQUEST_TIMEOUT, handle)
            .await
            .expect("task timed out")
            .expect("task panicked")?;
    }

    server_handle.abort();
    Ok(())
}

/// Tests that multiple independent connections with keep-alive don't interfere.
/// Each connection has its own handler instance with independent state.
#[tokio::test]
async fn test_framework_connection_isolation_with_keepalive() -> Result<()> {
    let port = find_available_port();
    let addr = format!("127.0.0.1:{}", port);

    let mut server = Server::new(addr.clone(), 1024 * 1024);
    server.set_protocol_command_handler_factory(|| Box::new(CounterHandler { count: 0 }));

    let server_handle = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(50)).await;

    // Create 3 independent clients with keep-alive
    let mut handles = Vec::new();
    for client_id in 0..3 {
        let addr_clone = addr.clone();
        handles.push(tokio::spawn(async move {
            let mut client = ClientTL::new(addr_clone);
            client.set_keep_alive(true).await;
            // Each client sends 5 requests on its own connection
            let mut last_count = 0;
            for _ in 0..5 {
                let response = timeout(REQUEST_TIMEOUT, client.handle_message(1, b"test"))
                    .await
                    .expect("request timed out")?;
                let count = usize::from_le_bytes(response[0..8].try_into().unwrap());
                // Counter should increment within each connection
                assert_eq!(
                    count,
                    last_count + 1,
                    "client {} expected {}, got {}",
                    client_id,
                    last_count + 1,
                    count
                );
                last_count = count;
            }
            Ok::<_, Error>(last_count)
        }));
    }

    // Each connection should have its own counter from 1 to 5
    for handle in handles {
        let final_count = timeout(REQUEST_TIMEOUT, handle)
            .await
            .expect("task timed out")
            .expect("task panicked")?;
        assert_eq!(final_count, 5);
    }

    server_handle.abort();
    Ok(())
}

/// Tests that WITHOUT keep-alive, each request creates a new handler instance.
/// This is the expected behavior: no connection reuse = no state persistence.
#[tokio::test]
async fn test_framework_no_keepalive_creates_new_handler() -> Result<()> {
    let port = find_available_port();
    let addr = format!("127.0.0.1:{}", port);

    let mut server = Server::new(addr.clone(), 1024 * 1024);
    server.set_protocol_command_handler_factory(|| Box::new(CounterHandler { count: 0 }));

    let server_handle = tokio::spawn(async move { server.run().await });
    sleep(Duration::from_millis(50)).await;

    // Without keep-alive, each request gets a NEW handler instance
    let mut client = ClientTL::new(addr);
    // Note: set_keep_alive is NOT called

    // Each request should return count=1 because a new handler is created each time
    for _ in 0..5 {
        let response = timeout(REQUEST_TIMEOUT, client.handle_message(1, b"test"))
            .await
            .expect("request timed out")?;
        let count = usize::from_le_bytes(response[0..8].try_into().unwrap());
        // Without keep-alive, every request starts fresh with count=1
        assert_eq!(
            count, 1,
            "Without keep-alive, each request should get a new handler starting at 1"
        );
    }

    server_handle.abort();
    Ok(())
}
