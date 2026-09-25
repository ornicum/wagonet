#[cfg(test)]
#[path = "common.rs"]
mod common;
use common::{spawn_server_with_shutdown, spawn_tls_server_with_shutdown};
use native_tls::{Identity, TlsAcceptor as NativeTlsAcceptor};
use std::sync::Arc;
use std::time::Duration;
use tokio_native_tls::TlsAcceptor;

use wagonet::client_tl::ClientTL;
use wagonet::client_tls::ClientTLS;
use wagonet::protocol_structs::ResponseStatus;
use wagonet::server_tl::ServerTL;
use wagonet::server_tls::ServerTLS;

fn create_test_tls_acceptor() -> TlsAcceptor {
    use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};

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

    TlsAcceptor::from(acceptor)
}

#[tokio::test]
async fn test_tcp_transport_success() {
    let (server_handle, server_shutdown, addr) = spawn_server_with_shutdown(|mut stream| async move {
        let (reader, writer) = stream.split();
        let mut server = ServerTL::new(reader, writer);
        eprintln!("[server] waiting for command");
        let (command, data_size) = server.read_command().await.unwrap();
        eprintln!("[server] got command={}, data_size={}", command, data_size);
        assert_eq!(command, 42);
        assert_eq!(data_size, 12);

        eprintln!("[server] waiting for data");
        let received_data = server.receive_data(data_size).await.unwrap();
        eprintln!("[server] got data: {:?}", received_data);
        assert_eq!(received_data, b"hello server");

        let response_body = b"hello client";
        eprintln!("[server] sending response");
        server
            .send_data(ResponseStatus::Ok.into(), Some(response_body))
            .await
            .unwrap();
        eprintln!("[server] response sent");
    }).await;

    let mut client = ClientTL::new(addr.to_string());
    let request_payload = b"hello server";

    eprintln!("[client] sending request");
    let response = client.handle_message(42, request_payload).await.unwrap();
    eprintln!("[client] got response: {:?}", response);
    assert_eq!(response, b"hello client");

    server_shutdown.notify_one();
    server_handle.await.unwrap();
}

#[tokio::test]
async fn test_tcp_transport_buffer_overflow() {
    let (server_handle, server_shutdown, addr) = spawn_server_with_shutdown(|mut stream| async move {
        let (reader, writer) = stream.split();
        let mut server = ServerTL::new(reader, writer);
        server.set_max_buffer_size(5);

        let res = server.read_command().await;
        assert!(res.is_err());
    }).await;

    let mut client = ClientTL::new(addr.to_string());
    let large_payload = b"this payload is too long for server";

    let response_res = client.handle_message(10, large_payload).await;

    assert!(response_res.is_err());

    server_shutdown.notify_one();
    server_handle.await.unwrap();
}

#[tokio::test]
async fn test_tcp_transport_no_answer() {
    let (server_handle, server_shutdown, addr) = spawn_server_with_shutdown(|mut stream| async move {
        let (reader, writer) = stream.split();
        let mut server = ServerTL::new(reader, writer);
        let (_, data_size) = server.read_command().await.unwrap();
        let received_data = server.receive_data(data_size).await.unwrap();
        assert_eq!(received_data, b"fire and forget");

        server
            .send_data(ResponseStatus::Ok.into(), None)
            .await
            .unwrap();
    }).await;

    let mut client = ClientTL::new(addr.to_string());
    let payload = b"fire and forget";

    let response = client
        .handle_message_with_no_answer(99, payload)
        .await
        .unwrap();

    assert!(response.is_empty());

    server_shutdown.notify_one();
    server_handle.await.unwrap();
}

#[tokio::test]
async fn test_tls_transport_success() {
    let acceptor = create_test_tls_acceptor();
    let (server_handle, server_shutdown, addr) = spawn_tls_server_with_shutdown(acceptor, |stream, acceptor| async move {
        let tls_stream = acceptor.accept(stream).await.unwrap();
        let mut server = ServerTLS::new(tls_stream);

        let (command, data_size) = server.read_command().await.unwrap();
        assert_eq!(command, 77);

        let received_data = server.receive_data(data_size).await.unwrap();
        assert_eq!(received_data, b"secure hello");

        server
            .send_data(ResponseStatus::Ok.into(), Some(b"secure reply"))
            .await
            .unwrap();
    }).await;

    let mut client = ClientTLS::new(addr.to_string());
    client.set_accept_invalid_certs(true);
    client.set_domain("example.org".to_string());

    let response = client.handle_message(77, b"secure hello").await.unwrap();
    assert_eq!(response, b"secure reply");

    server_shutdown.notify_one();
    server_handle.await.unwrap();
}

/// Test P1: set_keep_alive cycle true->false->true
/// Verifies that toggling keep_alive on a live connection works correctly:
/// - keep_alive(true): starts ping task on next connect()
/// - keep_alive(false): stops ping task, closes connection
/// - keep_alive(true) + handle_message(): reconnects, starts new ping task
#[tokio::test]
async fn keep_alive_cycle_true_false_true() {
    let ping_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ping_count_clone = ping_count.clone();

    let (server_handle, server_shutdown, addr) = spawn_server_with_shutdown(move |mut stream| {
        let ping_count = ping_count_clone.clone();
        async move {
            let (reader, writer) = stream.split();
            let mut server = ServerTL::new(reader, writer);
            server.set_timeout_config(wagonet::TimeoutConfig {
                read_header: Duration::from_millis(500),
                ..Default::default()
            });

            loop {
                let (command, data_size) = match server.read_command().await {
                    Ok((cmd, sz)) => (cmd, sz),
                    Err(_) => break,
                };

                if command == 0 && data_size == 0 {
                    ping_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    continue;
                }

                if data_size > 0 {
                    let _ = server.receive_data(data_size).await;
                }
                let _ = server.send_data(1, Some(b"OK")).await;
            }
        }
    }).await;

    let mut client = ClientTL::new(addr.to_string());
    client.set_keep_alive(true).await;
    client.set_timeout_config(wagonet::TimeoutConfig {
        read_header: Duration::from_millis(500),
        ping_interval: Duration::from_millis(200),
        ..Default::default()
    }).await;

    // Send one request to establish connection
    let _ = client.handle_message(1, b"hello").await.unwrap();

    // Phase 1: true - wait for pings
    tokio::time::sleep(Duration::from_millis(600)).await;
    let pings_phase1 = ping_count.load(std::sync::atomic::Ordering::SeqCst);
    assert!(pings_phase1 >= 2, "Phase 1 (true): Expected at least 2 pings, got {}", pings_phase1);

    // Phase 2: false - pings should stop
    client.set_keep_alive(false).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let pings_phase2 = ping_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(pings_phase2, pings_phase1, "Phase 2 (false): Ping count should not increase");

    // Phase 3: true again - pings should resume
    client.set_keep_alive(true).await;
    // Need to send a message to restart ping task (connect starts it)
    let _ = client.handle_message(2, b"restart").await.unwrap();
    
    // Wait longer for connection establishment and first ping
    tokio::time::sleep(Duration::from_millis(800)).await;

    let pings_phase3 = ping_count.load(std::sync::atomic::Ordering::SeqCst);
    // Just check that some pings happened after re-enable
    // (The exact count depends on timing, but should be > 0)
    assert!(pings_phase3 > pings_phase2, "Phase 3 (true): Expected more pings after re-enable, got {} -> {}", pings_phase2, pings_phase3);

    client.disconnect().await.unwrap();
    server_shutdown.notify_one();
    server_handle.await.unwrap();
}