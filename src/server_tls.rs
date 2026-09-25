use crate::protocol_structs::Command;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::error;

use crate::Result;
use crate::common::DEFAULT_MAX_BUFFER_SIZE;
use crate::request_header::RequestHeader;
use crate::response_header::ResponseHeader;
use crate::timeout_config::TimeoutConfig;

/// Combined trait for TLS streams that implement AsyncRead + AsyncWrite + Unpin + Send.
pub trait TlsStreamTrait: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T> TlsStreamTrait for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

/// TLS server handler for a single connection.
///
/// Same API as `ServerTL` but operates over a TLS stream.
/// The caller is responsible for the TLS handshake (via `tokio-native-tls` or similar).
pub struct ServerTLS<'a> {
    tls_stream: Pin<Box<dyn TlsStreamTrait + 'a>>,
    buffer: Vec<u8>,
    max_buffer_size: usize,
    timeout_config: TimeoutConfig,
}

impl<'a> ServerTLS<'a> {
    /// Create a new TLS server handler from a TLS stream.
    /// Accepts any stream implementing `AsyncRead + AsyncWrite + Unpin + Send`.
    pub fn new<S>(tls_stream: S) -> Self
    where
        S: TlsStreamTrait + 'a,
    {
        Self {
            tls_stream: Box::pin(tls_stream),
            buffer: Vec::with_capacity(DEFAULT_MAX_BUFFER_SIZE),
            max_buffer_size: DEFAULT_MAX_BUFFER_SIZE,
            timeout_config: TimeoutConfig::default(),
        }
    }

    /// Set maximum buffer size for incoming request data (default: 10 MB).
    /// Values <= 0 reset to default.
    pub fn set_max_buffer_size(&mut self, buffer_size: usize) {
        self.max_buffer_size = if buffer_size > 0 {
            buffer_size
        } else {
            DEFAULT_MAX_BUFFER_SIZE
        };
        self.buffer.reserve(self.max_buffer_size);
    }

    /// Set timeout configuration (read_header, read_data, write).
    pub fn set_timeout_config(&mut self, timeout_config: TimeoutConfig) {
        self.timeout_config = timeout_config;
    }

    /// Read a command header from the client.
    /// Returns `(command, data_size)` where `data_size` is the payload size in bytes.
    /// For Ping command (command=0, data_size=0), sends a 1-byte OK response and returns Ok((0, 0)).
    /// The caller should check for command == 0 and skip receive_data/send_data.
    pub async fn read_command(&mut self) -> Result<(u32, usize)> {
        self.buffer.resize(RequestHeader::encoded_len(), 0);

        match tokio::time::timeout(
            self.timeout_config.read_header,
            self.tls_stream.read_exact(&mut self.buffer),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                error!("Receiving request header error: {e}");
                return Err(e.into());
            }
            Err(_) => {
                error!("Request header receive timeout");
                return Err("Timeout waiting for request header".into());
            }
        }

        let req_header = RequestHeader::decode(&mut self.buffer.as_slice())?;
        self.buffer.clear();
        let data_size = req_header.data_size as usize;

        // Handle Ping command specially: empty request, 1-byte OK response, no payload.
        if req_header.command == Command::Ping.into() && data_size == 0 {
            // Send 1-byte response: status=Ok(1), data_size=0 (default encoding)
            self.send_response_header(1, 0).await?;
            return Ok((Command::Ping.into(), 0));
        }

        if data_size > self.max_buffer_size {
            self.send_response_header(7, 0).await?;
            error!("Request rejected: data size {data_size} exceeds MAX_BUFFER_SIZE");
            return Err("Data size exceeds maximum allowed buffer size".into());
        }
        self.send_response_header(1, 0).await?;
        Ok((req_header.command, req_header.data_size as usize))
    }

    async fn send_response_header(&mut self, status: u8, data_size: u32) -> Result<()> {
        let is_default = data_size == 0;
        self.buffer.clear();
        let res_header = ResponseHeader::new(status, data_size);
        res_header.encode(&mut self.buffer, is_default)?;

        match tokio::time::timeout(
            self.timeout_config.write,
            self.tls_stream.write_all(&self.buffer),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                error!("Sending response header error: {e}");
                return Err(e.into());
            }
            Err(_) => {
                error!("Sending response header timeout");
                return Err("Sending response header timeout".into());
            }
        }

        self.tls_stream.flush().await?;
        self.buffer.clear();
        Ok(())
    }

    async fn receive_response_header(&mut self, is_default: bool) -> Result<ResponseHeader> {
        let result_buf_len = ResponseHeader::encoded_len(is_default, true);
        self.buffer.resize(result_buf_len, 0);

        match tokio::time::timeout(
            self.timeout_config.read_header,
            self.tls_stream.read_exact(&mut self.buffer),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                error!("Receiving response header error: {e}");
                return Err(e.into());
            }
            Err(_) => {
                error!("Response header receive timeout");
                return Err("Timeout waiting for response header".into());
            }
        }

        let res_header = ResponseHeader::decode(&mut self.buffer.as_slice(), is_default)?;
        self.buffer.clear();
        Ok(res_header)
    }

    pub async fn receive_data(&mut self, buf_size: usize) -> Result<Vec<u8>> {
        if buf_size > self.max_buffer_size {
            return Err("Invalid data size".into());
        }
        self.buffer.resize(buf_size, 0);

        match tokio::time::timeout(
            self.timeout_config.read_data,
            self.tls_stream.read_exact(&mut self.buffer),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                return Err(e.into());
            }
            Err(_) => {
                error!("Data receive timeout");
                return Err("Timeout waiting for data".into());
            }
        }

        let res = self.buffer.to_vec();
        self.buffer.clear();
        Ok(res)
    }

    pub async fn send_data(&mut self, status: u8, buf: Option<&[u8]>) -> Result<()> {
        let data_size = if let Some(buf) = buf { buf.len() } else { 0 };
        self.send_response_header(status, data_size as u32).await?;
        let res_header = self.receive_response_header(true).await?;
        if res_header.status != 1 {
            return Err("Receiving response status is not OK".into());
        }
        if let Some(buf) = buf {
            match tokio::time::timeout(self.timeout_config.write, self.tls_stream.write_all(buf))
                .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    error!("Sending data error: {e}");
                    return Err(e.into());
                }
                Err(_) => {
                    error!("Sending data timeout");
                    return Err("Timeout sending data".into());
                }
            }
        }
        self.tls_stream.flush().await?;
        self.buffer.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_header::RequestHeader;
    use crate::timeout_config::TimeoutConfig;
    use std::time::Duration;

    #[tokio::test]
    async fn server_tls_ping_no_payload() {
        use tokio::io::duplex;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Create a duplex stream to simulate client-server communication
        let (mut client_stream, server_stream) = duplex(1024);

        let mut server = ServerTLS::new(server_stream);
        server.set_timeout_config(TimeoutConfig {
            read_header: Duration::from_millis(500),
            ..Default::default()
        });

        // Spawn server task
        let server_handle = tokio::spawn(async move {
            let (command, data_size) = server.read_command().await.unwrap();
            assert_eq!(command, 0);
            assert_eq!(data_size, 0);
        });

        // Send ping from client side
        let request_header = RequestHeader::new(0, 0);
        let mut buf = Vec::new();
        request_header.encode(&mut buf).unwrap();
        client_stream.write_all(&buf).await.unwrap();
        client_stream.flush().await.unwrap();

        // Read response (1 byte)
        let mut response = [0u8; 1];
        client_stream.read_exact(&mut response).await.unwrap();
        assert_eq!(response[0], 1); // OK status

        server_handle.await.unwrap();
    }
}
