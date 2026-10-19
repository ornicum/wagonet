use crate::protocol_command_handler::ProtocolCommandHandler;
use crate::protocol_handler::ProtocolHandler;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_native_tls::TlsAcceptor;
use tracing::{error, info};

pub struct Client {
    stream: Arc<TcpStream>,
    tls_acceptor: Option<Arc<TlsAcceptor>>,
    peer: SocketAddr,
    max_buffer_size: usize,
}

impl Client {
    pub fn new(stream: TcpStream, peer: SocketAddr, max_buffer_size: usize) -> Self {
        Self {
            stream: Arc::new(stream),
            tls_acceptor: None,
            peer,
            max_buffer_size,
        }
    }

    pub fn set_tls_acceptor(&mut self, acceptor: Arc<TlsAcceptor>) {
        self.tls_acceptor = Some(acceptor);
    }

    pub async fn run(
        &mut self,
        command_handler: Box<dyn ProtocolCommandHandler>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut protocol = ProtocolHandler::new(command_handler);
        if let Some(tls_acceptor) = &self.tls_acceptor {
            let stream = Arc::get_mut(&mut self.stream).unwrap();
            match tls_acceptor.accept(stream).await {
                Ok(tls_stream) => {
                    protocol.set_server_tls_params(tls_stream, self.max_buffer_size);
                    protocol.handle_command().await?;
                    Ok(())
                }
                Err(e) => {
                    error!("Error on accepting TLS connection: {}", e);
                    Err(e.into())
                }
            }
        } else {
            let (reader, writer) = Arc::get_mut(&mut self.stream).unwrap().split();
            protocol.set_server_tl_params(reader, writer, self.max_buffer_size);
            info!("Run protocol handler for {}", self.peer);
            protocol.handle_command().await?;
            Ok(())
        }
    }
}
