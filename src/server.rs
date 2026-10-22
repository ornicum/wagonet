use native_tls::{Identity, TlsAcceptor as NativeTlsAcceptor};
use std::error::Error;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_native_tls::TlsAcceptor;
use tracing::{error, info};

use crate::connection_handler::ConnectionHandler;
use crate::protocol_command_handler::ProtocolCommandHandler;

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

    pub fn set_secure(&mut self, identity: &[u8], password: &str) -> Result<(), Box<dyn Error>> {
        let identity = Identity::from_pkcs12(identity, password)?;
        let native_acceptor = NativeTlsAcceptor::builder(identity).build()?;
        let tls_acceptor = TlsAcceptor::from(native_acceptor);

        self.is_tls = true;
        self.tls_acceptor = Some(Arc::new(tls_acceptor));
        Ok(())
    }

    pub fn set_protocol_command_handler_factory<F>(&mut self, factory: F)
    where
        F: Fn() -> Box<dyn ProtocolCommandHandler> + Send + Sync + 'static,
    {
        self.protocol_command_handler_factory = Some(Box::new(factory));
    }

    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    pub async fn run(&mut self) -> Result<(), Box<dyn Error>> {
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
