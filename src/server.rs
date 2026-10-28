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
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::sleep;
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
///
/// Backoff delay after an accept error before retrying, to avoid a tight
/// CPU-burning spin on sustained failures (e.g. EMFILE).
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);
const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// High-level TCP/TLS server.
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
    drain_timeout: Duration,
    max_connections: usize,
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
    ///
    /// The size is stored as-is and validated per connection when the handler
    /// starts: an unsatisfiable value fails that connection with
    /// `Error::Config` rather than panicking.
    pub fn new(address: String, max_buffer_size: usize) -> Server {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            address,
            listener: None,
            is_tls: false,
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
            max_buffer_size,
            max_connections: 1024,
            tls_acceptor: None,
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
    ///
    /// # Security
    ///
    /// PKCS#12 password handling is the caller's responsibility: the library
    /// does not zeroize caller-owned memory after `set_secure` returns, so the
    /// password bytes remain in the caller's `String` backing for the process
    /// lifetime. Clear the password at the application layer once `set_secure`
    /// has returned (e.g. with the `zeroize` crate).
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
    /// Set the maximum number of concurrent connections.
    ///
    /// `0` means unlimited (no connection limit).
    ///
    /// When the limit is reached, the accept loop waits for a free slot.
    /// Excess clients wait in the OS listen backlog; they are **not**
    /// rejected.
    pub fn set_max_connections(&mut self, max_connections: usize) {
        self.max_connections = max_connections;
    }
    /// Set the maximum time to wait for in-flight connections to finish
    /// after shutdown is signaled.
    ///
    /// `0` means no timeout (wait forever).
    pub fn set_drain_timeout(&mut self, duration: Duration) {
        self.drain_timeout = duration;
    }

    /// Signal the server to stop accepting new connections and exit `Self::run`.
    ///
    /// `Self::run` will return after the accept loop stops and all in-flight
    /// connections have drained (or the drain timeout has expired).
    ///
    /// Shutdown is idempotent: calling `shutdown` multiple times, or sending
    /// through both `shutdown` and a `shutdown_handle`, is safe; the watch
    /// channel keeps the last value and each subscribed receiver observes the
    /// signal once. If no receiver remains (for example `run` has already
    /// returned and dropped `shutdown_rx`), the send failure is logged at
    /// `tracing::debug!` level ("shutdown signaled with no active receivers")
    /// instead of being silently dropped.
    ///
    /// This is a convenience method that consumes the internal sender. Use
    /// `Self::shutdown_handle` when the shutdown trigger must
    /// live in a different task or component (e.g. a signal handler).
    pub fn shutdown(&self) {
        if let Err(_) = self.shutdown_tx.send(true) {
            tracing::debug!("shutdown signaled with no active receivers");
        }
    }

    /// Clone the shutdown sender so that the shutdown signal can be sent from
    /// another task (e.g. a signal handler, admin endpoint, or test harness).
    ///
    /// Sending `true` through this handle causes `Self::run` to break
    /// out of its accept loop and return.
    ///
    /// Shutdown is idempotent: multiple sends through this handle or
    /// `Self::shutdown` are safe; the watch channel keeps the last value. If
    /// no receiver remains, the send failure is logged at `tracing::debug!`
    /// level by `Self::shutdown`.
    ///
    /// `Self::run` will return after the accept loop stops and all in-flight
    /// connections have drained (or the drain timeout has expired).
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
    /// `run` returns after the accept loop stops (due to shutdown) and all
    /// in-flight connections have drained, or the drain timeout has expired
    /// (in which case remaining connections are aborted).
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

        let semaphore: Option<Arc<Semaphore>> = if self.max_connections != 0 {
            Some(Arc::new(Semaphore::new(self.max_connections)))
        } else {
            None
        };

        let mut connection_handles: Vec<JoinHandle<()>> = Vec::new();

        loop {
            connection_handles.retain(|h| !h.is_finished());

            let permit = if let Some(ref sem) = semaphore {
                sem.clone().acquire_owned().await.ok()
            } else {
                None
            };
            let permit = match permit {
                Some(p) => p,
                None => break,
            };

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
                    connection_handles.push(tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(e) = client.run(protocol_command_handler).await {
                            error!("Connection handler failed for {}: {}", peer, e);
                        }
                    }));
                }
                Err(e) => {
                    error!("Accept error: {}", e);
                    // Back off before retrying to avoid a tight spin on
                    // sustained failures (e.g. EMFILE), but remain
                    // interruptible by shutdown.
                    let backoff = sleep(ACCEPT_ERROR_BACKOFF);
                    tokio::pin!(backoff);
                    tokio::select! {
                        _ = &mut backoff => {}
                        _ = self.shutdown_rx.changed() => {
                            if *self.shutdown_rx.borrow() {
                                info!("Shutdown signal received, stopping server.");
                                break;
                            }
                        }
                    }
                }
            }
        }
        // Drain in-flight connections
        let drain_start = std::time::Instant::now();
        let mut i = 0;
        while i < connection_handles.len() {
            let elapsed = drain_start.elapsed();
            if elapsed >= self.drain_timeout {
                for handle in connection_handles.drain(i..) {
                    handle.abort();
                }
                break;
            }
            let remaining = self.drain_timeout.saturating_sub(elapsed);
            match tokio::time::timeout(remaining, &mut connection_handles[i]).await {
                Ok(_) => i += 1,
                Err(_) => {
                    for handle in connection_handles.drain(i..) {
                        handle.abort();
                    }
                    break;
                }
            }
        }

        Ok(())
    }
}
