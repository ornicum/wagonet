use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_native_tls::TlsAcceptor;
use tracing::{error, info};

use crate::protocol_command_handler::ProtocolCommandHandler;
use crate::protocol_handler::ProtocolHandler;
use crate::server_tl::ServerTL;
use crate::server_tls::ServerTLS;
use crate::transport::Transport;

pub struct ConnectionHandler<'a> {
    stream: Option<TcpStream>,
    tls_acceptor: Option<Arc<TlsAcceptor>>,
    peer: SocketAddr,
    max_buffer_size: usize,
    shutdown_rx: watch::Receiver<bool>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> ConnectionHandler<'a> {
    pub fn new(
        stream: TcpStream,
        peer: SocketAddr,
        max_buffer_size: usize,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Self {
        Self {
            stream: Some(stream),
            tls_acceptor: None,
            peer,
            max_buffer_size,
            shutdown_rx,
            _marker: std::marker::PhantomData,
        }
    }

    pub fn set_tls_acceptor(&mut self, acceptor: Arc<TlsAcceptor>) {
        self.tls_acceptor = Some(acceptor);
    }

    pub async fn run(
        &mut self,
        command_handler: Box<dyn ProtocolCommandHandler>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let stream = self.stream.take().ok_or("stream already consumed")?;

        let transport: Box<dyn Transport> = if let Some(tls_acceptor) = &self.tls_acceptor {
            match tls_acceptor.accept(stream).await {
                Ok(tls_stream) => {
                    let mut tls = ServerTLS::new(tls_stream);
                    tls.set_max_buffer_size(self.max_buffer_size);
                    Box::new(tls)
                }
                Err(e) => {
                    error!("Error on accepting TLS connection: {}", e);
                    return Err(e.into());
                }
            }
        } else {
            let (reader, writer) = stream.into_split();
            let mut tl = ServerTL::new(reader, writer);
            tl.set_max_buffer_size(self.max_buffer_size);
            info!("Run protocol handler for {}", self.peer);
            Box::new(tl)
        };

        let mut protocol = ProtocolHandler::new(transport, command_handler);

        loop {
            tokio::select! {
                result = protocol.handle_next() => {
                    match result {
                        Ok(keep_alive) => {
                            if !keep_alive {
                                break;
                            }
                        }
                        Err(e) => {
                            error!("Protocol error: {}", e);
                            break;
                        }
                    }
                }
                _ = self.shutdown_rx.changed() => {
                    if *self.shutdown_rx.borrow() {
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}
