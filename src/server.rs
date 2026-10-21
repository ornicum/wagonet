use native_tls::{Identity, TlsAcceptor as NativeTlsAcceptor};
use std::error::Error;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_native_tls::TlsAcceptor;
use tracing::{error, info};

use crate::client::Client;
use crate::protocol_command_handler::ProtocolCommandHandler;

pub struct Server {
    address: String,
    listener: Option<TcpListener>,
    is_tls: bool,
    tls_acceptor: Option<Arc<TlsAcceptor>>,
    max_buffer_size: usize,
    protocol_command_handler_factory:
        Option<Box<dyn Fn() -> Box<dyn ProtocolCommandHandler> + Send + Sync + 'static>>,
}

impl Server {
    pub fn new(address: String, max_buffer_size: usize) -> Server {
        Self {
            address,
            listener: None,
            is_tls: false,
            tls_acceptor: None,
            max_buffer_size,
            protocol_command_handler_factory: None,
        }
    }

    pub fn set_secure(&mut self, identity: &[u8], password: &str) -> Result<(), Box<dyn Error>> {
        self.is_tls = true;
        let identity = Identity::from_pkcs12(identity, password)?;
        let native_acceptor = NativeTlsAcceptor::builder(identity).build()?;
        let tls_acceptor = TlsAcceptor::from(native_acceptor);
        self.tls_acceptor = Some(Arc::new(tls_acceptor));
        Ok(())
    }

    pub fn set_protocol_command_handler_factory<F>(&mut self, factory: F)
    where
        F: Fn() -> Box<dyn ProtocolCommandHandler> + Send + Sync + 'static,
    {
        self.protocol_command_handler_factory = Some(Box::new(factory));
    }

    pub async fn run(&mut self) {
        if self.protocol_command_handler_factory.is_none() {
            error!("Protocol Command Handler is not set.");
            panic!("Protocol Command Handler is not set.")
        }
        match TcpListener::bind(&self.address).await {
            Ok(listener) => {
                self.listener = Some(listener);
                info!("Listening on {}", self.address);
            }
            Err(e) => {
                error!("Error: {}", e);
                return;
            }
        }

        while let Ok((stream, peer)) = self.listener.as_ref().unwrap().accept().await {
            let mut client = Client::new(stream, peer, self.max_buffer_size);
            if self.is_tls
                && let Some(tls_acceptor) = &self.tls_acceptor
            {
                client.set_tls_acceptor(Arc::clone(tls_acceptor));
            }
            let protocol_command_handler =
                self.protocol_command_handler_factory.as_ref().unwrap()();
            tokio::spawn(async move {
                let _ = client.run(protocol_command_handler).await;
            });
        }
    }
}
