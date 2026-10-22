use tracing::error;

use crate::Result;
use crate::protocol_command_handler::ProtocolCommandHandler;
use crate::protocol_structs::ResponseStatus;
use crate::transport::Transport;

pub struct ProtocolHandler {
    transport: Box<dyn Transport>,
    command_handler: Box<dyn ProtocolCommandHandler>,
}

impl ProtocolHandler {
    pub fn new(
        transport: Box<dyn Transport>,
        command_handler: Box<dyn ProtocolCommandHandler>,
    ) -> Self {
        Self {
            transport,
            command_handler,
        }
    }

    pub async fn handle_next(&mut self) -> Result<bool> {
        let command_info = self.transport.read_command().await?;

        let (command, data_size) = match command_info {
            Some(info) => info,
            None => return Ok(true),
        };

        let data = self.transport.receive_data(data_size).await?;

        match self.command_handler.run(command, data).await {
            Ok((status, response_data)) => {
                if let Err(e) = self
                    .transport
                    .send_response(status.into(), Some(&response_data))
                    .await
                {
                    error!("Error sending response: {}", e);
                    return Err(e);
                }

                if status == ResponseStatus::Finish {
                    return Ok(false);
                }
            }
            Err(e) => {
                error!("Command handler error: {}", e);
                if let Err(send_err) = self
                    .transport
                    .send_response(ResponseStatus::Error.into(), None)
                    .await
                {
                    error!("Error sending error response: {}", send_err);
                }
                return Err(e);
            }
        }

        Ok(true)
    }
}
