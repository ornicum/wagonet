use native_tls::TlsConnector as NativeTlsConnector;
use socket2::SockRef;
use std::error::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_native_tls::{TlsConnector, TlsStream};
use tracing::error;

use crate::common::DEFAULT_MAX_BUFFER_SIZE;
use crate::request_header::RequestHeader;
use crate::response_header::ResponseHeader;
use crate::timeout_config::TimeoutConfig;

/// TLS client with connection reuse, certificate validation, and configurable timeouts.
///
/// # Features
/// - **TLS encryption** via `native-tls` / `tokio-native-tls`
/// - **Certificate validation enabled by default** (`accept_invalid_certs: false`)
/// - **SNI support** via `set_domain()` (required for certificate verification)
/// - **Connection reuse** via `keep_alive` (enabled with `set_keep_alive(true)`)
/// - **Bounded retry**: 1 retry on transient failure when keep-alive is enabled
/// - **Configurable timeouts**: connect, read_header, read_data, write
/// - **TCP keep-alive**: OS-level probes via `socket2`
/// - **No-answer mode**: fire-and-forget commands via `set_command_has_answer(false)`
/// - **Buffer protection**: configurable max buffer size (default 10 MB)
///
/// # Example
/// ```rust,no_run
/// use wagonet::{ClientTLS, timeout_config::{TimeoutConfig, KeepAliveConfig}};
/// use std::time::Duration;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
///     let mut client = ClientTLS::new("example.org:443".to_string());
///
///     // TLS certificate validation is ON by default
///     client.set_domain("example.org".to_string());
///
///     // For self-signed certs in development only:
///     // client.set_accept_invalid_certs(true);
///
///     let response = client.handle_message(1, b"hello").await?;
///     println!("Response: {:?}", String::from_utf8_lossy(&response));
///
///     Ok(())
/// }
/// ```
#[derive(Debug)]
pub struct ClientTLS {
    accept_invalid_certs: bool,
    address: String,
    domain: String,
    tls_stream: Option<TlsStream<TcpStream>>,
    command_has_answer: bool,
    buffer: Vec<u8>,
    max_buffer_size: usize,
    timeout_config: TimeoutConfig,
    keep_alive: bool,
}

impl ClientTLS {
    /// Create a new TLS client with the given address (e.g., "example.org:443").
    /// Certificate validation is enabled by default. Call `set_domain()` before connecting.
    pub fn new(address: String) -> Self {
        Self {
            accept_invalid_certs: false,
            address,
            domain: "example.org".to_string(),
            tls_stream: None,
            command_has_answer: true,
            buffer: Vec::with_capacity(DEFAULT_MAX_BUFFER_SIZE),
            max_buffer_size: DEFAULT_MAX_BUFFER_SIZE,
            timeout_config: TimeoutConfig::default(),
            keep_alive: false,
        }
    }

    /// Set maximum buffer size for incoming responses (default: 10 MB).
    /// Values <= 0 reset to default.
    pub fn set_max_buffer_size(&mut self, max_buffer_size: usize) {
        self.max_buffer_size = if max_buffer_size > 0 {
            max_buffer_size
        } else {
            DEFAULT_MAX_BUFFER_SIZE
        };
        self.buffer.reserve(self.max_buffer_size);
    }

    /// Enable/disable no-answer mode (fire-and-forget).
    /// When `false`, `handle_message` returns immediately with empty `Vec<u8>`
    /// without waiting for a response header.
    pub fn set_command_has_answer(&mut self, command_has_answer: bool) {
        self.command_has_answer = command_has_answer;
    }

    /// Allow invalid TLS certificates (self-signed, expired, wrong host).
    /// **Only for development/testing!** Default is `false` (strict validation).
    pub fn set_accept_invalid_certs(&mut self, accept_invalid_certs: bool) {
        self.accept_invalid_certs = accept_invalid_certs;
    }

    /// Set the domain name for SNI and certificate verification.
    /// Must match the server's certificate CommonName or SAN.
    /// Required for certificate validation to work correctly.
    pub fn set_domain(&mut self, domain: String) {
        self.domain = domain;
    }

    /// Enable/disable connection reuse (keep-alive).
    /// When enabled, the TLS connection stays open after a request for reuse.
    /// When disabled (default), each `handle_message` opens a new connection.
    pub fn set_keep_alive(&mut self, enabled: bool) {
        self.keep_alive = enabled;
    }

    /// Set timeout configuration (connect, read_header, read_data, write, keep_alive).
    pub fn set_timeout_config(&mut self, timeout_config: TimeoutConfig) {
        self.timeout_config = timeout_config;
    }

    /// Explicitly establish a TLS connection with retry logic.
    /// Retries up to 10 times with 1s delay between attempts.
    /// Called automatically by `handle_message` if not already connected.
    pub async fn connect(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut connector = NativeTlsConnector::builder();
        connector.danger_accept_invalid_certs(self.accept_invalid_certs);
        let connector = connector.build()?;
        let connector = TlsConnector::from(connector);
        let mut tries: u8 = 0;
        while tries < 10 {
            match tokio::time::timeout(
                self.timeout_config.connect,
                TcpStream::connect(&self.address),
            )
            .await
            {
                Ok(Ok(stream)) => {
                    if self.keep_alive {
                        let sock_ref = SockRef::from(&stream);
                        if let Some(ka) = self.timeout_config.keep_alive.as_ref() {
                            let tcp_keepalive = socket2::TcpKeepalive::new()
                                .with_time(ka.time)
                                .with_interval(ka.interval);
                            let _ = sock_ref.set_tcp_keepalive(&tcp_keepalive);
                        } else {
                            let _ = sock_ref.set_keepalive(true);
                        }
                    }
                    self.tls_stream = Some(connector.connect(self.domain.as_str(), stream).await?);
                    return Ok(());
                }
                Ok(Err(e)) => {
                    error!("Connection error: {e}");
                }
                Err(_) => {
                    error!("Connection timeout");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            tries += 1;
        }
        if self.tls_stream.is_none() && tries >= 10 {
            error!("Connection error, max retries reached");
            return Err("Connection error, max retries reached".into());
        }
        Ok(())
    }

    /// Gracefully close the TLS connection.
    /// Sends shutdown and clears the internal stream.
    pub async fn disconnect(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let stream = self.tls_stream.as_mut().ok_or("Not connected")?;
        if let Err(e) = stream.shutdown().await {
            error!("Disconnection error: {e}");
            self.tls_stream = None;
            return Err(e.into());
        }
        self.tls_stream = None;
        Ok(())
    }

    async fn send_request_header(
        &mut self,
        command: u32,
        data_size: u32,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let request_header = RequestHeader::new(command, data_size);
        self.buffer.clear();
        request_header.encode(&mut self.buffer)?;
        let stream = self.tls_stream.as_mut().ok_or("Not connected")?;

        match tokio::time::timeout(self.timeout_config.write, stream.write_all(&self.buffer)).await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                error!("Sending request header error: {e}");
                return Err(e.into());
            }
            Err(_) => {
                error!("Sending request header timeout");
                return Err("Timeout sending request header".into());
            }
        }

        stream.flush().await?;
        self.buffer.clear();
        Ok(())
    }

    async fn send_response_header(
        &mut self,
        status: u8,
        data_size: u32,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let is_default = data_size == 0;
        let response_header = ResponseHeader::new(status, data_size);
        self.buffer.clear();
        response_header.encode(&mut self.buffer, is_default)?;
        let stream = self.tls_stream.as_mut().ok_or("Not connected")?;

        match tokio::time::timeout(self.timeout_config.write, stream.write_all(&self.buffer)).await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                error!("Sending response header error: {e}");
                return Err(e.into());
            }
            Err(_) => {
                error!("Sending response header timeout");
                return Err("Timeout sending response header".into());
            }
        }

        stream.flush().await?;
        self.buffer.clear();
        Ok(())
    }

    async fn receive_response_header(
        &mut self,
        is_default: bool,
    ) -> Result<ResponseHeader, Box<dyn Error + Send + Sync>> {
        let result_buf_size = ResponseHeader::encoded_len(is_default, self.command_has_answer);
        self.buffer.resize(result_buf_size, 0);
        let stream = self.tls_stream.as_mut().ok_or("Not connected")?;

        match tokio::time::timeout(
            self.timeout_config.read_header,
            stream.read_exact(&mut self.buffer),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                error!("Receiving request header error: {e}");
                return Err(e.into());
            }
            Err(_) => {
                error!("Response header receive timeout");
                return Err("Timeout waiting for response header".into());
            }
        }

        let decode_as_default = is_default || !self.command_has_answer;
        let response_header =
            ResponseHeader::decode(&mut self.buffer.as_slice(), decode_as_default)?;
        self.buffer.clear();
        Ok(response_header)
    }

    async fn receive_message_data_size(&mut self) -> Result<usize, Box<dyn Error + Send + Sync>> {
        match self.receive_response_header(false).await {
            Ok(res_header) => {
                let status = res_header.status;
                if status != 1 {
                    return Err(format!("Response status is not Ok: {status}").into());
                }
                let data_usize = res_header.data_size as usize;
                if data_usize > self.max_buffer_size {
                    return Err("Data size is more then max buffer size".into());
                }
                Ok(data_usize)
            }
            Err(e) => Err(e),
        }
    }

    async fn receive_message(&mut self) -> Result<&Vec<u8>, Box<dyn Error + Send + Sync>> {
        let data_size = self.receive_message_data_size().await?;
        self.send_response_header(1, 0).await?;
        if data_size == 0 {
            self.buffer.clear();
            return Ok(&self.buffer);
        }
        self.buffer.resize(data_size, 0);
        let stream = self.tls_stream.as_mut().ok_or("Not connected")?;

        match tokio::time::timeout(
            self.timeout_config.read_data,
            stream.read_exact(&mut self.buffer),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                error!("Receiving message error: {e}");
                return Err(e.into());
            }
            Err(_) => {
                error!("Message receive timeout");
                return Err("Timeout waiting for message data".into());
            }
        }

        Ok(&self.buffer)
    }

    async fn send_message(
        &mut self,
        command: u32,
        data: &[u8],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.send_request_header(command, data.len() as u32).await?;
        self.receive_response_header(true).await?;
        let stream = self.tls_stream.as_mut().ok_or("Not connected")?;

        match tokio::time::timeout(self.timeout_config.write, stream.write_all(data)).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                error!("Sending message error: {e}");
                return Err(e.into());
            }
            Err(_) => {
                error!("Sending message timeout");
                return Err("Timeout sending message data".into());
            }
        }

        stream.flush().await?;
        self.buffer.clear();
        Ok(())
    }

    /// Send a request and receive a response (primary API).
    ///
    /// Behavior:
    /// - If `keep_alive=true` and a connection exists, reuses it.
    /// - Otherwise establishes a new TLS connection (with retry).
    /// - Sends request header + data, reads response header + data.
    /// - On transient failure with `keep_alive=true`: clears stream, reconnects, retries once.
    /// - Returns response payload bytes.
    pub async fn handle_message(
        &mut self,
        command: u32,
        request_data: &[u8],
    ) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        // If keep_alive is enabled and already connected, reuse connection
        if self.keep_alive && self.tls_stream.is_some() {
            // Connection already exists, reuse it
        } else {
            self.connect().await?;
        }

        // Try to send/receive, if fails due to connection issue, reconnect and retry once
        let result = self.try_send_receive(command, request_data).await;
        match result {
            Ok(response) => Ok(response),
            Err(e) => {
                if self.keep_alive {
                    // Try to reconnect and retry once
                    error!("Message exchange failed, reconnecting: {}", e);
                    self.tls_stream = None;
                    self.connect().await?;
                    self.try_send_receive(command, request_data).await
                } else {
                    Err(e)
                }
            }
        }
    }

    async fn try_send_receive(
        &mut self,
        command: u32,
        request_data: &[u8],
    ) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        let mut buf = vec![];
        self.send_message(command, request_data).await?;
        match self.receive_message().await {
            Ok(response) => buf.extend_from_slice(response),
            Err(e) => return Err(e),
        }
        self.buffer.clear();
        Ok(buf)
    }

    /// Fire-and-forget: send request without waiting for response.
    ///
    /// Temporarily sets `command_has_answer=false`, calls `handle_message`,
    /// then restores the original setting. Returns empty `Vec<u8>` on success.
    /// Use `set_command_has_answer(false)` + `handle_message` for persistent no-answer mode.
    pub async fn handle_message_with_no_answer(
        &mut self,
        command: u32,
        request_data: &[u8],
    ) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        self.command_has_answer = false;
        let res = self.handle_message(command, request_data).await;
        self.command_has_answer = true;
        res
    }
}
