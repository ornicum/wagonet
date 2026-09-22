use socket2::SockRef;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::sleep;
use tracing::error;

use crate::common::DEFAULT_MAX_BUFFER_SIZE;
use crate::request_header::RequestHeader;
use crate::response_header::ResponseHeader;
use crate::timeout_config::TimeoutConfig;
use crate::{Error, Result};
/// Plain TCP client with connection reuse and configurable timeouts.
///
/// # Features
/// - **Connection reuse** via `keep_alive` (enabled with `set_keep_alive(true)`)
/// - **Bounded retry**: 1 retry on transient failure when keep-alive is enabled
/// - **Configurable timeouts**: connect, read_header, read_data, write
/// - **TCP keep-alive**: OS-level probes via `socket2` (idle time, interval)
/// - **No-answer mode**: fire-and-forget commands via `set_command_has_answer(false)`
/// - **Buffer protection**: configurable max buffer size (default 10 MB)
///
/// # Example
/// ```rust,no_run
/// use wagonet::{ClientTL, timeout_config::{TimeoutConfig, KeepAliveConfig}};
/// use std::time::Duration;
///
/// #[tokio::main]
/// async fn main() -> wagonet::Result<()> {
///     let mut client = ClientTL::new("127.0.0.1:8080".to_string());
///     client.set_keep_alive(true);
///     client.set_timeout_config(TimeoutConfig {
///         keep_alive: Some(KeepAliveConfig {
///             time: Duration::from_secs(60),
///             interval: Duration::from_secs(15),
///         }),
///         ..Default::default()
///     });
///
///     let response = client.handle_message(1, b"hello").await?;
///     println!("Response: {:?}", String::from_utf8_lossy(&response));
///     Ok(())
/// }
/// ```
#[derive(Debug)]
pub struct ClientTL {
    address: String,
    stream: Option<TcpStream>,
    command_has_answer: bool,
    buffer: Vec<u8>,
    max_buffer_size: usize,
    timeout_config: TimeoutConfig,
    keep_alive: bool,
}

impl ClientTL {
    /// Create a new client with the given address (e.g., "127.0.0.1:8080").
    /// Connection is established lazily on first `handle_message` or via `connect()`.
    pub fn new(address: String) -> Self {
        Self {
            address,
            stream: None,
            command_has_answer: true,
            buffer: Vec::with_capacity(DEFAULT_MAX_BUFFER_SIZE),
            max_buffer_size: DEFAULT_MAX_BUFFER_SIZE,
            timeout_config: TimeoutConfig::default(),
            keep_alive: false,
        }
    }

    /// Set maximum buffer size for incoming responses (default: 10 MB).
    /// Values <= 0 reset to default.
    pub fn set_max_buffer_size(&mut self, buffer_size: usize) {
        self.max_buffer_size = if buffer_size > 0 {
            buffer_size
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

    /// Enable/disable connection reuse (keep-alive).
    /// When enabled, the connection stays open after a request for reuse.
    /// When disabled (default), each `handle_message` opens a new connection.
    pub fn set_keep_alive(&mut self, enabled: bool) {
        self.keep_alive = enabled;
    }

    /// Set timeout configuration (connect, read_header, read_data, write, keep_alive).
    pub fn set_timeout_config(&mut self, timeout_config: TimeoutConfig) {
        self.timeout_config = timeout_config;
    }

    /// Explicitly establish a TCP connection with retry logic.
    /// Retries up to 10 times with 1s delay between attempts.
    /// Called automatically by `handle_message` if not already connected.
    pub async fn connect(&mut self) -> Result<()> {
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
                    self.stream = Some(stream);
                    return Ok(());
                }
                Ok(Err(e)) => {
                    error!("Connection error: {e}");
                }
                Err(_) => {
                    error!("Connection timeout");
                }
            }
            sleep(Duration::from_secs(1)).await;
            tries += 1;
        }
        if self.stream.is_none() && tries >= 10 {
            error!("Connection error, max retries reached");
            return Err("Connection error, max retries reached".into());
        }
        Ok(())
    }

    /// Gracefully close the TCP connection.
    /// Sends shutdown and clears the internal stream.
    pub async fn disconnect(&mut self) -> Result<()> {
        let stream = self.stream.as_mut().ok_or("Not connected")?;
        if let Err(e) = stream.shutdown().await {
            error!("Disconnection error: {e}");
            self.stream = None;
            return Err(e.into());
        }
        self.stream = None;
        Ok(())
    }

    async fn send_request_header(&mut self, command: u32, data_size: u32) -> Result<()> {
        let request_header = RequestHeader::new(command, data_size);
        request_header.encode(&mut self.buffer)?;
        let stream = self.stream.as_mut().ok_or(Error::NotConnected)?;

        match tokio::time::timeout(self.timeout_config.write, stream.write_all(&self.buffer)).await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                error!("Sending request header error: {e}");
                return Err(e.into());
            }
            Err(_) => {
                error!("Sending request header timeout");
                return Err(Error::Timeout("Sending request header".to_string()));
            }
        }

        stream.flush().await?;
        self.buffer.clear();
        Ok(())
    }

    async fn send_response_header(&mut self, status: u8, data_size: u32) -> Result<()> {
        let is_default = data_size == 0;
        let response_header = ResponseHeader::new(status, data_size);
        response_header.encode(&mut self.buffer, is_default)?;
        let stream = self.stream.as_mut().ok_or(Error::NotConnected)?;

        match tokio::time::timeout(self.timeout_config.write, stream.write_all(&self.buffer)).await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                error!("Sending response header error: {e}");
                return Err(e.into());
            }
            Err(_) => {
                error!("Sending response header timeout");
                return Err(Error::Timeout("Sending response header".to_string()));
            }
        }

        stream.flush().await?;
        self.buffer.clear();
        Ok(())
    }

    async fn receive_response_header(&mut self, is_default: bool) -> Result<ResponseHeader> {
        let result_buf_size = ResponseHeader::encoded_len(is_default, self.command_has_answer);
        self.buffer.resize(result_buf_size, 0);
        let stream = self.stream.as_mut().ok_or(Error::NotConnected)?;

        match tokio::time::timeout(
            self.timeout_config.read_header,
            stream.read_exact(&mut self.buffer),
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
                return Err(Error::Timeout("Waiting for response header".to_string()));
            }
        }

        let decode_as_default = is_default || !self.command_has_answer;
        let response_header =
            ResponseHeader::decode(&mut self.buffer.as_slice(), decode_as_default)?;
        self.buffer.clear();
        Ok(response_header)
    }

    async fn receive_message_data_size(&mut self) -> Result<usize> {
        match self.receive_response_header(false).await {
            Ok(res_header) => {
                let status = res_header.status;
                if status != 1 {
                    return Err(Error::ResponseError { status });
                }
                let data_usize = res_header.data_size as usize;
                if data_usize > self.max_buffer_size {
                    return Err(Error::BufferOverflow {
                        expected: data_usize,
                        limit: self.max_buffer_size,
                    });
                }
                Ok(data_usize)
            }
            Err(e) => Err(e),
        }
    }

    async fn receive_message(&mut self) -> Result<Vec<u8>> {
        let data_size = self.receive_message_data_size().await?;
        self.send_response_header(1, 0).await?;
        if data_size == 0 {
            self.buffer.clear();
            return Ok(Vec::new());
        }
        self.buffer.resize(data_size, 0);
        let stream = self.stream.as_mut().ok_or(Error::NotConnected)?;
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
                return Err(Error::Timeout("Waiting for message data".to_string()));
            }
        }

        Ok(self.buffer.to_vec())
    }

    async fn send_message(&mut self, command: u32, data: &[u8]) -> Result<()> {
        self.send_request_header(command, data.len() as u32).await?;
        self.receive_response_header(true).await?;
        let stream = self.stream.as_mut().ok_or(Error::NotConnected)?;

        match tokio::time::timeout(self.timeout_config.write, stream.write_all(data)).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                error!("Sending message error: {e}");
                return Err(e.into());
            }
            Err(_) => {
                error!("Sending message timeout");
                return Err(Error::Timeout("Sending message data".to_string()));
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
    /// - Otherwise establishes a new connection (with retry).
    /// - Sends request header + data, reads response header + data.
    /// - On transient failure with `keep_alive=true`: clears stream, reconnects, retries once.
    /// - Returns response payload bytes.
    pub async fn handle_message(&mut self, command: u32, request_data: &[u8]) -> Result<Vec<u8>> {
        // If keep_alive is enabled and already connected, reuse connection
        if self.keep_alive && self.stream.is_some() {
            // Connection already exists, reuse it
        } else {
            self.connect().await?;
        }

        let result = self.try_send_receive(command, request_data).await;
        match result {
            Ok(response) => Ok(response),
            Err(e) => {
                if self.keep_alive {
                    error!("Message exchange failed, reconnecting: {}", e);
                    self.stream = None;
                    self.connect().await?;
                    self.try_send_receive(command, request_data).await
                } else {
                    Err(e)
                }
            }
        }
    }

    async fn try_send_receive(&mut self, command: u32, request_data: &[u8]) -> Result<Vec<u8>> {
        let mut buf = vec![];
        self.send_message(command, request_data).await?;
        match self.receive_message().await {
            Ok(response) => buf.extend_from_slice(&response),
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
    ) -> Result<Vec<u8>> {
        self.command_has_answer = false;
        let res = self.handle_message(command, request_data).await;
        self.command_has_answer = true;
        res
    }
}
