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
#[non_exhaustive]
pub enum Error {
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
pub mod ping;
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
pub use timeout_config::TimeoutConfig;
