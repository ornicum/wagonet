use native_tls::{Identity, TlsAcceptor as NativeTlsAcceptor};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_native_tls::TlsAcceptor;
use wagonet::{ClientTLS, TimeoutConfig, server_tls::ServerTLS};

/// TLS Ping example demonstrating application-level keep-alive over TLS.
///
/// This example shows how the ping protocol keeps a TLS connection alive
/// during idle periods, preventing server-side read_header timeouts.
///
/// Run with: cargo run --example ping_tls_example
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialize tracing for debug output
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    // Generate self-signed certificate for testing
    let mut params = CertificateParams::default();
    params.not_before = rcgen::date_time_ymd(2026, 1, 1);
    params.not_after = rcgen::date_time_ymd(2036, 1, 1);

    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "example.org");
    params.distinguished_name = dn;
    params.subject_alt_names = vec![SanType::DnsName("example.org".try_into().unwrap())];

    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let cert = params.self_signed(&key_pair).unwrap();

    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    let identity = Identity::from_pkcs8(cert_pem.as_bytes(), key_pem.as_bytes()).unwrap();
    let acceptor = NativeTlsAcceptor::builder(identity).build().unwrap();
    let tls_acceptor = TlsAcceptor::from(acceptor);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let server_ready = Arc::new(Notify::new());
    let server_ready_clone = server_ready.clone();

    // Connection counter to verify no reconnection occurs
    let connection_count = Arc::new(AtomicUsize::new(0));
    let connection_count_clone = connection_count.clone();
    let tls_acceptor_clone = tls_acceptor.clone();

    // Server: read_header timeout = 500ms, but client pings every 100ms
    let server_handle = tokio::spawn(async move {
        server_ready_clone.notify_one();

        let (mut stream, _) = listener.accept().await.unwrap();
        let tls_stream = tls_acceptor_clone.accept(&mut stream).await.unwrap();
        connection_count_clone.fetch_add(1, Ordering::SeqCst);
        eprintln!(
            "[server] New TLS connection accepted (total: {})",
            connection_count_clone.load(Ordering::SeqCst)
        );

        let mut server = ServerTLS::new(tls_stream);

        // Short read_header timeout to demonstrate ping effectiveness
        server.set_timeout_config(TimeoutConfig {
            read_header: Duration::from_millis(500),
            ..Default::default()
        });

        loop {
            eprintln!("[server] Waiting for command...");
            let (command, data_size) = match server.read_command().await {
                Ok((cmd, sz)) => (cmd, sz),
                Err(e) => {
                    eprintln!("[server] read_command error: {e}");
                    break;
                }
            };

            if command == 0 && data_size == 0 {
                eprintln!("[server] Received PING, responded OK");
                continue; // Ping handled by read_command, no payload
            }

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
    let mut client = ClientTLS::new(addr.clone());
    client.set_accept_invalid_certs(true); // Accept self-signed cert
    client.set_domain("example.org".to_string());
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

    eprintln!("[client] SUCCESS: TLS Connection survived idle period thanks to ping!");
    Ok(())
}
