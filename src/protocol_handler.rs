use std::error::Error;
use tokio::net::TcpStream;
use tokio::net::tcp::{ReadHalf, WriteHalf};
use tokio_native_tls::TlsStream;
use tracing::error;

use crate::protocol_command_handler::ProtocolCommandHandler;
use crate::protocol_structs::{Command as WagonetCommand, ResponseStatus};
use crate::server_tl::ServerTL;
use crate::server_tls::ServerTLS;

pub struct ProtocolHandler<'a> {
    command_handler: Box<dyn ProtocolCommandHandler>,
    server_tl: Option<ServerTL<'a>>,
    server_tls: Option<ServerTLS<'a>>,
}

impl<'a> ProtocolHandler<'a> {
    pub fn new(command_handler: Box<dyn ProtocolCommandHandler>) -> Self {
        Self {
            command_handler,
            server_tl: None,
            server_tls: None,
        }
    }

    pub fn set_server_tl_params(
        &mut self,
        reader: ReadHalf<'a>,
        writer: WriteHalf<'a>,
        max_buffer_size: usize,
    ) {
        self.server_tl = Some(ServerTL::new(reader, writer));
        self.server_tl
            .as_mut()
            .unwrap()
            .set_max_buffer_size(max_buffer_size);
    }

    pub fn set_server_tls_params(
        &mut self,
        tls_stream: TlsStream<&'a mut TcpStream>,
        max_buffer_size: usize,
    ) {
        self.server_tls = Some(ServerTLS::new(tls_stream));
        self.server_tls
            .as_mut()
            .unwrap()
            .set_max_buffer_size(max_buffer_size);
    }

    async fn get_command_info_tl(
        &mut self,
    ) -> Result<(u32, Vec<u8>), Box<dyn Error + Send + Sync>> {
        match self.server_tl.as_mut().unwrap().read_command().await {
            Ok(Some((com, data_size))) => {
                match self
                    .server_tl
                    .as_mut()
                    .unwrap()
                    .receive_data(data_size)
                    .await
                {
                    Ok(data) => Ok((com, data)),
                    Err(e) => {
                        error!("TL Error: {}", e);
                        Err(e.into())
                    }
                }
            }
            Ok(None) => Ok((WagonetCommand::Ping.into(), Vec::new())),
            Err(e) => {
                error!("TL Error: {}", e);
                Err(e.into())
            }
        }
    }

    async fn get_command_info_tls(
        &mut self,
    ) -> Result<(u32, Vec<u8>), Box<dyn Error + Send + Sync>> {
        match self.server_tls.as_mut().unwrap().read_command().await {
            Ok(Some((com, data_size))) => {
                match self
                    .server_tls
                    .as_mut()
                    .unwrap()
                    .receive_data(data_size)
                    .await
                {
                    Ok(data) => Ok((com, data)),
                    Err(e) => {
                        error!("TLS Error: {}", e);
                        Err(e.into())
                    }
                }
            }
            Ok(None) => Ok((WagonetCommand::Ping.into(), Vec::new())),
            Err(e) => {
                error!("TLS Error: {}", e);
                Err(e.into())
            }
        }
    }

    async fn get_command_info(&mut self) -> Result<(u32, Vec<u8>), Box<dyn Error + Send + Sync>> {
        if self.server_tl.is_none() && self.server_tls.is_none() {
            panic!("Server is not configured.");
        }
        if self.server_tl.is_some() {
            self.get_command_info_tl().await
        } else {
            self.get_command_info_tls().await
        }
    }

    pub async fn handle_command(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        if self.server_tl.is_none() && self.server_tls.is_none() {
            panic!("Server is not configured.");
        }
        let (command, command_data) = self.get_command_info().await?;
        if command == WagonetCommand::Ping.into() {
            return Ok(())
        }
        match self.command_handler.run(command, command_data).await {
            Ok((status, data)) => {
                if self.server_tl.is_some()
                    && let Err(e) = self
                        .server_tl
                        .as_mut()
                        .unwrap()
                        .send_data(status.into(), Some(&data))
                        .await
                {
                    error!("Error: {}", e);
                    return Err(e.into());
                }
                if self.server_tls.is_some()
                    && let Err(e) = self
                        .server_tls
                        .as_mut()
                        .unwrap()
                        .send_data(status.into(), Some(&data))
                        .await
                {
                    error!("Error: {}", e);
                    return Err(e.into());
                }
            }
            Err(e) => {
                if self.server_tl.is_some()
                    && let Err(e) = self
                        .server_tl
                        .as_mut()
                        .unwrap()
                        .send_data(ResponseStatus::Error.into(), None)
                        .await
                {
                    error!("Error: {}", e);
                    return Err(e.into());
                }
                if self.server_tls.is_some()
                    && let Err(e) = self
                        .server_tls
                        .as_mut()
                        .unwrap()
                        .send_data(ResponseStatus::Error.into(), None)
                        .await
                {
                    error!("Error: {}", e);
                    return Err(e.into());
                }
                error!("Error: {}", e);
                return Err(e);
            }
        }
        Ok(())
    }
}
