use crate::Result;
use crate::protocol_command_handler::ProtocolCommandHandler;
use crate::protocol_structs::ResponseStatus;
use crate::transport::Transport;

/// Drives the request-response protocol over an established [`Transport`].
///
/// Owns a transport (TCP or TLS connection behind the [`Transport`] trait)
/// and a user-provided [`ProtocolCommandHandler`] with application logic.
pub struct ProtocolHandler {
    transport: Box<dyn Transport>,
    command_handler: Box<dyn ProtocolCommandHandler>,
}

impl ProtocolHandler {
    /// Create a handler from a transport and a command handler.
    pub fn new(
        transport: Box<dyn Transport>,
        command_handler: Box<dyn ProtocolCommandHandler>,
    ) -> Self {
        Self {
            transport,
            command_handler,
        }
    }

    /// Process a single request-response cycle.
    ///
    /// Returns:
    /// - `Ok(true)` when the connection should stay open (keep-alive). This
    ///   also covers transport-level pings (handled internally) and command
    ///   handler errors (the client receives an `Error` status, the session
    ///   continues).
    /// - `Ok(false)` when the command handler returned
    ///   [`ResponseStatus::Finish`]: the session is complete.
    /// - `Err(e)` on transport-level failures; the connection is unusable.
    pub async fn handle_next(&mut self) -> Result<bool> {
        // `None` means the transport handled a keep-alive ping internally.
        let (command, data_size) = match self.transport.read_command().await? {
            Some(info) => info,
            None => return Ok(true),
        };

        let data = self.transport.receive_data(data_size).await?;

        match self.command_handler.run(command, data).await {
            Ok((status, response_data)) => {
                self.transport
                    .send_response(status.into(), Some(&response_data))
                    .await?;

                if status == ResponseStatus::Finish {
                    return Ok(false);
                }
            }
            Err(_) => {
                // Send an empty payload instead of `None` so that clients in
                // answer mode receive a full 5-byte header and stay in sync.
                let _ = self
                    .transport
                    .send_response(ResponseStatus::Error.into(), Some(&[]))
                    .await;
                // Application errors do not terminate the connection.
            }
        }

        Ok(true)
    }
}
