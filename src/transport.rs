//! Internal abstraction over TCP and TLS server transports.
//!
//! The [`Transport`] trait lets the framework (`ProtocolHandler`, `ConnectionHandler`)
//! work uniformly over plain TCP and TLS without duplicating protocol logic.
//! It is not part of the public API surface; users interact with the server
//! through [`Server`] and [`ProtocolCommandHandler`](crate::ProtocolCommandHandler)
//! instead.

use async_trait::async_trait;

use crate::Result;

/// Abstraction over a single server-side connection (TCP or TLS).
///
/// Implemented by `ServerTL` and `ServerTLS`. The framework expects these
/// semantics:
///
/// - Frames use the library's custom binary protocol.
/// - Keep-alive pings (command `0`) are consumed internally and never reach
///   the command handler.
/// - The transport owns the connection; dropping it closes the socket.
#[async_trait]
pub trait Transport: Send {
    /// Read the next request header.
    ///
    /// Returns `Some((command, data_size))` for a normal request, where
    /// `data_size` is the length of the payload that follows. Returns
    /// `Ok(None)` for a keep-alive ping (the ping response has already been
    /// sent to the client).
    ///
    /// # Errors
    ///
    /// Returns an error on timeout, I/O failure, or protocol violation
    /// (e.g. ping with non-zero `data_size`, oversized payload request).
    async fn read_command(&mut self) -> Result<Option<(u32, u32)>>;

    /// Read exactly `size` bytes of request payload from the client.
    ///
    /// `size` must match the `data_size` returned by the most recent
    /// [`read_command`](Self::read_command) call.
    ///
    /// # Errors
    ///
    /// Returns an error on timeout or I/O failure.
    async fn receive_data(&mut self, size: u32) -> Result<Vec<u8>>;

    /// Send a response frame to the client.
    ///
    /// `data`:
    /// - `None` — no-answer mode (1-byte response header, used by the client
    ///   for fire-and-forget commands).
    /// - `Some(bytes)` — normal response with a 5-byte header. Use
    ///   `Some(&[])` for an empty payload when a full header is required.
    ///
    /// # Errors
    ///
    /// Returns an error on timeout or I/O failure.
    async fn send_response(&mut self, status: u8, data: Option<&[u8]>) -> Result<()>;

    /// Flush the underlying writer.
    ///
    /// Called during connection teardown and after every protocol exchange
    /// to make sure buffered bytes reach the client.
    async fn shutdown(&mut self) -> Result<()>;
}
