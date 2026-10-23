//! User-facing trait for handling application commands.
//!
//! Implementors describe what the server should do for each incoming request.
//! A new instance is created per connection, so handler state is private to
//! that connection.
//!
//! # Example
//!
//! ```rust
//! use async_trait::async_trait;
//! use wagonet::{ProtocolCommandHandler, Result};
//! use wagonet::protocol_structs::ResponseStatus;
//!
//! struct EchoHandler {
//!     requests_served: u64,
//! }
//!
//! #[async_trait]
//! impl ProtocolCommandHandler for EchoHandler {
//!     async fn run(&mut self, command: u32, data: Vec<u8>) -> Result<(ResponseStatus, Vec<u8>)> {
//!         self.requests_served += 1;
//!         // Status::Finish closes the connection after this response.
//!         // Status::Error signals an application-level failure but keeps the
//!         // connection open for further requests.
//!         Ok((ResponseStatus::Ok, data))
//!     }
//! }
//! ```

use async_trait::async_trait;

use crate::Result;
use crate::protocol_structs::ResponseStatus;

/// Application command handler.
///
/// Implemented by user code and registered with the [`Server`] through
/// [`Server::set_protocol_command_handler_factory`]. One instance per
/// accepted connection.
///
/// Implementations must be `Send + Sync + 'static` because the server spawns
/// a tokio task per connection and may move the handler across threads.
#[async_trait]
pub trait ProtocolCommandHandler: Send + Sync + 'static {
    /// Handle a single incoming command.
    ///
    /// # Arguments
    ///
    /// - `command` — the numeric command identifier from the request header.
    ///   Command `0` is reserved by the protocol for keep-alive pings and is
    ///   handled internally by the transport layer; user handlers see `command >= 1`.
    /// - `data` — the decoded payload bytes. May be empty.
    ///
    /// # Returns
    ///
    /// - `Ok((ResponseStatus::Ok, payload))` — send `payload` back to the
    ///   client with an OK status.
    /// - `Ok((ResponseStatus::Finish, payload))` — send `payload` back and
    ///   close the connection after this response.
    /// - `Ok((ResponseStatus::Error, payload))` — send `payload` back with an
    ///   error status. The connection stays open.
    /// - `Err(_)` — the framework sends an `Error` status with an empty
    ///   payload. The connection stays open so the client can retry.
    async fn run(&mut self, command: u32, data: Vec<u8>) -> Result<(ResponseStatus, Vec<u8>)>;
}
