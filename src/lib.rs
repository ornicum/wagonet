//! wagonet - High-performance async TCP/TLS transport library with custom protocol.
//!
// # License
// //
// // Licensed under either of
// // - MIT License ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
// // - Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
// //   at your option.
// //
//! ## Architecture
//!
//! See [docs/architecture.md](https://github.com/ornicum/wagonet/blob/main/docs/architecture.md)
//! for component diagrams, state machines, sequence diagrams, and security analysis.
//!
//! ## Features
//!
//! - TCP and TLS transports with custom binary protocol
//! - Configurable timeouts (connect, read_header, read_data, write)
//! - **TCP keep-alive** via socket2 (configurable idle time, probe interval)
//! - **Connection reuse** with bounded retry (1 retry) and health-aware reconnection
//! - Secure TLS defaults (certificate validation enabled by default)
//! - Symmetric client/server logic
//! - No-answer mode for fire-and-forget commands
//! - Configurable buffer limits (10 MB default)
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use wagonet::{ClientTL, timeout_config::{TimeoutConfig, KeepAliveConfig}};
//! use std::time::Duration;
//!
//! #[tokio::main]
//! async fn main() -> wagonet::Result<()> {
//!     let mut client = ClientTL::new("127.0.0.1:8080".to_string());
//!     client.set_keep_alive(true);
//!     client.set_timeout_config(TimeoutConfig {
//!         keep_alive: Some(KeepAliveConfig {
//!             time: std::time::Duration::from_secs(60),
//!             interval: std::time::Duration::from_secs(15),
//!         }),
//!         ..Default::default()
//!     });
//!     
//!     let response = client.handle_message(1, b"hello").await?;
//!     println!("Response: {:?}", response);
//!     Ok(())
//! }
//! ```
use thiserror::Error;

/// Unified error type for wagonet operations.
#[derive(Debug, Error)]
pub enum Error {
    /// I/O error (connection, read, write, TLS handshake).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Tokio timeout elapsed.
    #[error("Timeout: {0}")]
    Timeout(String),

    /// TLS error during handshake or I/O.
    #[error("TLS error: {0}")]
    Tls(#[from] native_tls::Error),

    /// Protocol error (malformed header, unexpected data).
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// Buffer size exceeded configured limit.
    #[error("Buffer overflow: expected {expected}, limit {limit}")]
    BufferOverflow { expected: usize, limit: usize },

    /// Connection not established.
    #[error("Not connected")]
    NotConnected,

    /// Response status indicates an error.
    #[error("Response error: status {status}")]
    ResponseError { status: u8 },

    /// Invalid configuration.
    #[error("Invalid config: {0}")]
    Config(String),

    /// Other errors.
    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),

    /// Generic error message.
    #[error("{0}")]
    Msg(String),
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::Msg(s.to_string())
    }
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Msg(s)
    }
}

/// Result type alias for wagonet operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;

pub mod client_tl;
pub mod client_tls;
mod common;
pub mod protocol_structs;
mod request_header;
mod response_header;
pub mod server_tl;
pub mod server_tls;
pub mod timeout_config;

pub use client_tl::ClientTL;
pub use client_tls::ClientTLS;
pub use server_tl::ServerTL;
pub use server_tls::ServerTLS;
#[cfg(test)]
mod integration_tests {
    use native_tls::{Identity, TlsAcceptor as NativeTlsAcceptor};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::Notify;
    use tokio_native_tls::TlsAcceptor;

    use crate::client_tl::ClientTL;
    use crate::client_tls::ClientTLS;
    use crate::protocol_structs::ResponseStatus;
    use crate::server_tl::ServerTL;
    use crate::server_tls::ServerTLS;

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
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        let server_ready = Arc::new(Notify::new());
        let server_ready_clone = server_ready.clone();

        let server_handle = tokio::spawn(async move {
            server_ready_clone.notify_one();
            let (mut stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = tokio::io::split(&mut stream);
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
        });

        server_ready.notified().await;

        let mut client = ClientTL::new(addr);
        let request_payload = b"hello server";

        eprintln!("[client] sending request");
        let response = client.handle_message(42, request_payload).await.unwrap();
        eprintln!("[client] got response: {:?}", response);
        assert_eq!(response, b"hello client");

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_tcp_transport_buffer_overflow() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        let server_handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = tokio::io::split(&mut stream);
            let mut server = ServerTL::new(reader, writer);
            server.set_max_buffer_size(5);

            let res = server.read_command().await;
            assert!(res.is_err());
        });

        let mut client = ClientTL::new(addr);
        let large_payload = b"this payload is too long for server";

        let response_res = client.handle_message(10, large_payload).await;

        assert!(response_res.is_err());

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_tcp_transport_no_answer() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server_handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = tokio::io::split(&mut stream);
            let mut server = ServerTL::new(reader, writer);
            let (_, data_size) = server.read_command().await.unwrap();
            let received_data = server.receive_data(data_size).await.unwrap();
            assert_eq!(received_data, b"fire and forget");

            server
                .send_data(ResponseStatus::Ok.into(), None)
                .await
                .unwrap();
        });

        let mut client = ClientTL::new(addr);
        let payload = b"fire and forget";

        let response = client
            .handle_message_with_no_answer(99, payload)
            .await
            .unwrap();

        assert!(response.is_empty());

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_tls_transport_success() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let acceptor = create_test_tls_acceptor();

        let server_handle = tokio::spawn({
            let acceptor = acceptor.clone();
            async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let tls_stream = acceptor.accept(&mut stream).await.unwrap();
                let mut server = ServerTLS::new(tls_stream);

                let (command, data_size) = server.read_command().await.unwrap();
                assert_eq!(command, 77);

                let received_data = server.receive_data(data_size).await.unwrap();
                assert_eq!(received_data, b"secure hello");

                server
                    .send_data(ResponseStatus::Ok.into(), Some(b"secure reply"))
                    .await
                    .unwrap();
            }
        });

        let mut client = ClientTLS::new(addr);
        client.set_accept_invalid_certs(true);
        client.set_domain("example.org".to_string());

        let response = client.handle_message(77, b"secure hello").await.unwrap();
        assert_eq!(response, b"secure reply");

        server_handle.await.unwrap();
    }
}
