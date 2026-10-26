//! High-level TCP/TLS server with graceful shutdown and per-connection
//! `ProtocolCommandHandler` dispatching.
//!
//! # Example
//!
//! ```rust,no_run
//! use async_trait::async_trait;
//! use wagonet::{Server, ProtocolCommandHandler, Result};
//! use wagonet::protocol_structs::ResponseStatus;
//!
//! struct MyHandler;
//!
//! #[async_trait]
//! impl ProtocolCommandHandler for MyHandler {
//!     async fn run(&mut self, command: u32, data: Vec<u8>) -> Result<(ResponseStatus, Vec<u8>)> {
//!         Ok((ResponseStatus::Ok, data)) // echo
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
//!     let mut server = Server::new("127.0.0.1:8080".to_string(), 10 * 1024 * 1024);
//!     server.set_protocol_command_handler_factory(|| Box::new(MyHandler));
//!
//!     // In another task: server.shutdown();
//!     server.run().await
//! }
//! ```

use native_tls::{Identity, TlsAcceptor as NativeTlsAcceptor};
use std::error::Error;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_native_tls::TlsAcceptor;
use tracing::{error, info};

use crate::connection_handler::ConnectionHandler;
use crate::protocol_command_handler::ProtocolCommandHandler;

/// High-level TCP/TLS server.
///
/// Binds to an address, accepts connections, and dispatches each connection to a
/// new `ConnectionHandler` that runs the request-response protocol against a
/// user-provided `ProtocolCommandHandler`.
///
/// Supports:
/// - Plain TCP (default) and TLS (via `Self::set_secure`).
/// - Graceful shutdown through `Self::shutdown` or
///   `Self::shutdown_handle`.
/// - Per-connection command handler instances via a user-supplied factory closure.
pub struct Server {
    address: String,
    listener: Option<TcpListener>,
    is_tls: bool,
    tls_acceptor: Option<Arc<TlsAcceptor>>,
    max_buffer_size: usize,
    protocol_command_handler_factory:
        Option<Box<dyn Fn() -> Box<dyn ProtocolCommandHandler> + Send + Sync + 'static>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl Server {
    /// Create a new server that will bind to `address` (e.g. `"127.0.0.1:8080"`)
    /// with the given maximum request payload size in bytes (0 falls back to the
    /// library default of 10 MB).
    ///
    /// No listener is created until `Self::run` is awaited, so a
    /// `ProtocolCommandHandler` factory must be configured via
    /// `Self::set_protocol_command_handler_factory`
    /// before `run` is called.
    pub fn new(address: String, max_buffer_size: usize) -> Server {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            address,
            listener: None,
            is_tls: false,
            tls_acceptor: None,
            max_buffer_size,
            protocol_command_handler_factory: None,
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// Enable TLS with a PKCS#12 identity.
    ///
    /// `identity` is the raw PKCS#12 bytes (usually read from a `.p12` file)
    /// and `password` is the decryption password. Once called, every accepted
    /// connection goes through a TLS handshake before any protocol frames are
    /// exchanged.
    ///
    /// # Errors
    ///
    /// Returns an error if the identity cannot be parsed or the native TLS
    /// acceptor cannot be built (e.g. invalid password, malformed certificate).
    /// The server is left unchanged in that case.
    pub fn set_secure(
        &mut self,
        identity: &[u8],
        password: &str,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let identity = Identity::from_pkcs12(identity, password)?;
        let native_acceptor = NativeTlsAcceptor::builder(identity).build()?;
        let tls_acceptor = TlsAcceptor::from(native_acceptor);

        self.is_tls = true;
        self.tls_acceptor = Some(Arc::new(tls_acceptor));
        Ok(())
    }

    /// Register a factory that creates a fresh `ProtocolCommandHandler` for
    /// every accepted connection.
    ///
    /// The factory is invoked once per connection so that each client gets its
    /// own handler state. The closure must be `Send + Sync + 'static` because
    /// connections are processed by spawned tasks on the tokio runtime.
    ///
    /// This must be called before `Self::run`; otherwise `run` returns
    /// an error immediately.
    pub fn set_protocol_command_handler_factory<F>(&mut self, factory: F)
    where
        F: Fn() -> Box<dyn ProtocolCommandHandler> + Send + Sync + 'static,
    {
        self.protocol_command_handler_factory = Some(Box::new(factory));
    }

    /// Signal the server to stop accepting new connections and exit `Self::run`.
    ///
    /// This is a convenience method that consumes the internal sender. Use
    /// `Self::shutdown_handle` when the shutdown trigger must
    /// live in a different task or component (e.g. a signal handler).
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Clone the shutdown sender so that the shutdown signal can be sent from
    /// another task (e.g. a signal handler, admin endpoint, or test harness).
    ///
    /// Sending `true` through this handle causes `Self::run` to break
    /// out of its accept loop and return.
    pub fn shutdown_handle(&self) -> watch::Sender<bool> {
        self.shutdown_tx.clone()
    }

    /// Bind to the configured address and serve until the shutdown signal is
    /// received or an unrecoverable error occurs.
    ///
    /// For every accepted connection:
    /// 1. A fresh `ProtocolCommandHandler` is created via the registered factory.
    /// 2. The connection is handed to a new tokio task that runs the protocol
    ///    (TCP or TLS, depending on `Self::set_secure`).
    /// 3. Errors from individual connections are logged and do **not** stop the
    ///    server.
    ///
    /// # Errors
    ///
    /// - No factory has been registered (call
    ///   `Self::set_protocol_command_handler_factory`
    ///   first).
    /// - The address cannot be bound (already in use, insufficient permissions).
    ///
    /// Transient accept failures are logged and retried; they do not return
    /// from `run`.
    pub async fn run(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        if self.protocol_command_handler_factory.is_none() {
            return Err("Protocol Command Handler is not set.".into());
        }
        match TcpListener::bind(&self.address).await {
            Ok(listener) => {
                self.listener = Some(listener);
                info!("Listening on {}", self.address);
            }
            Err(e) => {
                error!("Error binding to {}: {}", self.address, e);
                return Err(Box::new(e));
            }
        }

        loop {
            let accept_result = tokio::select! {
                res = self.listener.as_ref().unwrap().accept() => res,
                _ = self.shutdown_rx.changed() => {
                    if *self.shutdown_rx.borrow() {
                        info!("Shutdown signal received, stopping server.");
                        break;
                    }
                    continue;
                }
            };

            match accept_result {
                Ok((stream, peer)) => {
                    let mut client = ConnectionHandler::new(
                        stream,
                        peer,
                        self.max_buffer_size,
                        self.shutdown_rx.clone(),
                    );
                    if self.is_tls
                        && let Some(tls_acceptor) = &self.tls_acceptor
                    {
                        client.set_tls_acceptor(Arc::clone(tls_acceptor));
                    }
                    let protocol_command_handler =
                        self.protocol_command_handler_factory.as_ref().unwrap()();
                    tokio::spawn(async move {
                        if let Err(e) = client.run(protocol_command_handler).await {
                            error!("Connection handler failed for {}: {}", peer, e);
                        }
                    });
                }
                Err(e) => {
                    error!("Accept error: {}", e);
                }
            }
        }
        Ok(())
    }
}
