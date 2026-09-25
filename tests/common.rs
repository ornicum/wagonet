//! Common test utilities for wagonet integration tests.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Spawns a mock server with graceful shutdown support.
///
/// Returns a tuple of:
/// - `JoinHandle<()>`: The server task handle (await to ensure clean shutdown)
/// - `Arc<Notify>`: Shutdown signal - call `.notify_one()` to stop the server
/// - `SocketAddr`: The address the server is listening on
///
/// The `handler` is called once per accepted connection with the `TcpStream`.
/// It should handle the full protocol exchange for that connection.
pub async fn spawn_server_with_shutdown<F, Fut>(
    handler: F,
) -> (JoinHandle<()>, Arc<Notify>, SocketAddr)
where
    F: Fn(TcpStream) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let shutdown = Arc::new(Notify::new());
    let shutdown_clone = shutdown.clone();

    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_clone.notified() => {
                    break;
                }
                accept_result = listener.accept() => {
                    let (stream, _) = match accept_result {
                        Ok(s) => s,
                        Err(_) => break,
                    };
                    handler(stream).await;
                }
            }
        }
    });

    (handle, shutdown, addr)
}

/// Spawns a TLS server with graceful shutdown support.
/// The handler receives the raw TcpStream and the TlsAcceptor, and is responsible
/// for performing the TLS handshake and handling the connection.
pub async fn spawn_tls_server_with_shutdown<F, Fut>(
    acceptor: tokio_native_tls::TlsAcceptor,
    handler: F,
) -> (JoinHandle<()>, Arc<Notify>, SocketAddr)
where
    F: Fn(tokio::net::TcpStream, tokio_native_tls::TlsAcceptor) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let shutdown = Arc::new(Notify::new());
    let shutdown_clone = shutdown.clone();

    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_clone.notified() => {
                    break;
                }
                accept_result = listener.accept() => {
                    let (stream, _) = match accept_result {
                        Ok(s) => s,
                        Err(_) => break,
                    };
                    handler(stream, acceptor.clone()).await;
                }
            }
        }
    });

    (handle, shutdown, addr)
}
