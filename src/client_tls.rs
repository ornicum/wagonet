use native_tls::TlsConnector as NativeTlsConnector;
use socket2::SockRef;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_native_tls::{TlsConnector, TlsStream};
use tracing::{error, warn};

use crate::Result;
use crate::common::DEFAULT_MAX_BUFFER_SIZE;
use crate::ping::PingState;
use crate::request_header::RequestHeader;
use crate::response_header::ResponseHeader;
use crate::timeout_config::TimeoutConfig;

/// TLS client with connection reuse, ping keep-alive, certificate validation, and configurable timeouts.
///
/// # Features
/// - **TLS encryption** via `native-tls` / `tokio-native-tls`
/// - **Certificate validation enabled by default** (`accept_invalid_certs: false`)
/// - **SNI support** via `set_domain()` (required for certificate verification)
/// - **Connection reuse** via `keep_alive` (enabled with `set_keep_alive(true)`)
/// - **Application-level ping keep-alive**: periodic empty requests to prevent server timeout
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
///     client.set_keep_alive(true).await;
///     client.set_timeout_config(TimeoutConfig {
///         keep_alive: Some(KeepAliveConfig {
///             time: Duration::from_secs(60),
///             interval: Duration::from_secs(15),
///         }),
///         ping_interval: Duration::from_secs(30),
///         ..Default::default()
///     }).await;
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
    // When keep_alive is enabled, stream is shared with the ping task via Arc<Mutex>.
    // When keep_alive is disabled, the Mutex is still used but no ping task runs.
    tls_stream: Arc<Mutex<Option<TlsStream<TcpStream>>>>,
    command_has_answer: bool,
    buffer: Vec<u8>,
    max_buffer_size: usize,
    pub timeout_config: TimeoutConfig,
    keep_alive: bool,
    /// Ping state for background keep-alive task.
    ping_state: Arc<PingState>,
}

impl ClientTLS {
    /// Create a new TLS client with the given address (e.g., "example.org:443").
    /// Certificate validation is enabled by default. Call `set_domain()` before connecting.
    pub fn new(address: String) -> Self {
        let timeout_config = TimeoutConfig::default();
        let ping_config = crate::ping::PingConfig {
            interval: timeout_config.ping_interval,
            enabled: timeout_config.ping_interval > Duration::ZERO,
        };
        Self {
            accept_invalid_certs: false,
            address,
            domain: "example.org".to_string(),
            tls_stream: Arc::new(Mutex::new(None)),
            command_has_answer: true,
            buffer: Vec::with_capacity(DEFAULT_MAX_BUFFER_SIZE),
            max_buffer_size: DEFAULT_MAX_BUFFER_SIZE,
            timeout_config,
            keep_alive: false,
            ping_state: PingState::new(ping_config, TimeoutConfig::default()),
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
    ///
    /// **Note:** When disabling keep-alive (`enabled = false`), this method will
    /// stop the background ping task by calling `stop_ping_task()`, which may
    /// block for up to 5 seconds (the default ping task shutdown timeout) if the
    /// ping task is stuck in network I/O. The stream is also cleared, so the next
    /// `handle_message` will establish a new connection.
    pub async fn set_keep_alive(&mut self, enabled: bool) {
        if !enabled && self.keep_alive {
            // Stop ping task before disabling keep_alive
            self.ping_state.stop_ping_task().await;
            // Clear the stream so next handle_message will reconnect
            *self.tls_stream.lock().await = None;
        }
        self.keep_alive = enabled;
        let config_arc = self.ping_state.config();
        let mut config = config_arc.lock().await;
        config.enabled = enabled && config.interval > Duration::ZERO;
    }

    /// Set timeout configuration (connect, read_header, read_data, write, keep_alive, ping_interval).
    pub async fn set_timeout_config(&mut self, timeout_config: TimeoutConfig) -> Result<()> {
        // Use builder to automatically clamp ping_interval
        let config = timeout_config.clone().with_ping_interval(timeout_config.ping_interval);
        config.validate()?;
        let ping_enabled = self.keep_alive && config.ping_interval > Duration::ZERO;
        let config_arc = self.ping_state.config();
        let mut ping_config = config_arc.lock().await;
        ping_config.interval = config.ping_interval;
        ping_config.enabled = ping_enabled;
        // Also update timeout_config in ping_state for dynamic read_header
        let timeout_arc = self.ping_state.timeout_config();
        *timeout_arc.lock().await = config.clone();
        self.timeout_config = config;
        Ok(())
    }


    /// Explicitly establish a TLS connection with retry logic.
    /// Retries up to 10 times with 1s delay between attempts.
    /// Called automatically by `handle_message` if not already connected.
    pub async fn connect(&mut self) -> Result<()> {
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
                            if let Err(e) = sock_ref.set_tcp_keepalive(&tcp_keepalive) {
                                warn!(
                                    "Failed to set TCP keepalive: {}. Probes may not work as expected.",
                                    e
                                );
                            }
                        } else {
                            let _ = sock_ref.set_keepalive(true);
                        }
                    }
                    let tls_stream = connector.connect(self.domain.as_str(), stream).await?;
                    *self.tls_stream.lock().await = Some(tls_stream);

                    // Start ping task if keep_alive is enabled
                    if self.keep_alive && self.ping_state.is_enabled().await {
                        self.ping_state
                            .clone()
                            .start_ping_task(self.tls_stream.clone())
                            .await;
                    }
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
        if self.tls_stream.lock().await.is_none() && tries >= 10 {
            error!("Connection error, max retries reached");
            return Err("Connection error, max retries reached".into());
        }
        Ok(())
    }
    /// Gracefully close the TLS connection.
    /// Sends shutdown and clears the internal stream.
    ///
    /// **Note:** This method stops the background ping task by calling
    /// `stop_ping_task()`, which may block for up to 5 seconds (the default
    /// ping task shutdown timeout) if the ping task is stuck in network I/O.
    pub async fn disconnect(&mut self) -> Result<()> {
        // Stop ping task first
        self.ping_state.stop_ping_task().await;

        let mut stream_guard = self.tls_stream.lock().await;
        if let Some(stream) = stream_guard.as_mut()
            && let Err(e) = stream.shutdown().await
        {
            error!("Disconnection error: {e}");
            *stream_guard = None;
            return Err(e.into());
        }
        *stream_guard = None;
        Ok(())
    }

    async fn send_request_header_locked(
        stream: &mut TlsStream<TcpStream>,
        command: u32,
        data_size: u32,
        timeout: Duration,
        buffer: &mut Vec<u8>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let request_header = RequestHeader::new(command, data_size);
        buffer.clear();
        request_header.encode(buffer)?;

        match tokio::time::timeout(timeout, stream.write_all(buffer)).await {
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
        buffer.clear();
        Ok(())
    }

    async fn send_response_header_locked(
        stream: &mut TlsStream<TcpStream>,
        status: u8,
        data_size: u32,
        timeout: Duration,
        buffer: &mut Vec<u8>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let is_default = data_size == 0;
        let response_header = ResponseHeader::new(status, data_size);
        buffer.clear();
        response_header.encode(buffer, is_default)?;

        match tokio::time::timeout(timeout, stream.write_all(buffer)).await {
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
        buffer.clear();
        Ok(())
    }

    async fn receive_response_header_locked(
        stream: &mut TlsStream<TcpStream>,
        is_default: bool,
        command_has_answer: bool,
        timeout: Duration,
        buffer: &mut Vec<u8>,
    ) -> Result<ResponseHeader, Box<dyn std::error::Error + Send + Sync>> {
        let result_buf_size = ResponseHeader::encoded_len(is_default, command_has_answer);
        buffer.resize(result_buf_size, 0);

        match tokio::time::timeout(timeout, stream.read_exact(buffer)).await {
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

        let decode_as_default = is_default || !command_has_answer;
        let response_header = ResponseHeader::decode(&mut buffer.as_slice(), decode_as_default)?;
        buffer.clear();
        Ok(response_header)
    }

    async fn send_message_locked(
        stream: &mut TlsStream<TcpStream>,
        command: u32,
        data: &[u8],
        timeout_config: &TimeoutConfig,
        command_has_answer: bool,
        buffer: &mut Vec<u8>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Self::send_request_header_locked(
            stream,
            command,
            data.len() as u32,
            timeout_config.write,
            buffer,
        )
        .await?;
        Self::receive_response_header_locked(
            stream,
            true,
            command_has_answer,
            timeout_config.read_header,
            buffer,
        )
        .await?;

        match tokio::time::timeout(timeout_config.write, stream.write_all(data)).await {
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
        buffer.clear();
        Ok(())
    }

    async fn receive_message_locked(
        stream: &mut TlsStream<TcpStream>,
        timeout_config: &TimeoutConfig,
        command_has_answer: bool,
        max_buffer_size: usize,
        buffer: &mut Vec<u8>,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let data_size = {
            let res_header = Self::receive_response_header_locked(
                stream,
                false,
                command_has_answer,
                timeout_config.read_header,
                buffer,
            )
            .await?;
            let status = res_header.status;
            if status != 1 {
                return Err(format!("Response status is not Ok: {status}").into());
            }
            let data_usize = res_header.data_size as usize;
            if data_usize > max_buffer_size {
                return Err("Data size is more then max buffer size".into());
            }
            data_usize
        };

        // Send ACK for response header
        Self::send_response_header_locked(stream, 1, 0, timeout_config.write, buffer).await?;

        if data_size == 0 {
            buffer.clear();
            return Ok(Vec::new());
        }

        buffer.resize(data_size, 0);
        match tokio::time::timeout(timeout_config.read_data, stream.read_exact(buffer)).await {
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

        Ok(buffer.to_vec())
    }

    /// Send a request and receive a response (primary API).
    ///
    /// Behavior:
    /// - If `keep_alive=true` and a connection exists, reuses it.
    /// - Otherwise establishes a new TLS connection (with retry).
    /// - Sends request header + data, reads response header + data.
    /// - On transient failure with `keep_alive=true`: clears stream, reconnects, retries once.
    /// - Returns response payload bytes.
    pub async fn handle_message(&mut self, command: u32, request_data: &[u8]) -> Result<Vec<u8>> {
        if !self.keep_alive || self.tls_stream.lock().await.is_none() {
            self.connect().await?;
        }

        // Acquire in-flight lock for the entire request-response cycle
        let _in_flight_guard = self.ping_state.acquire_in_flight().await;
        let mut stream_guard = self.tls_stream.lock().await;
        let stream = match stream_guard.as_mut() {
            Some(s) => s,
            None => return Err("Not connected".into()),
        };

        let result = async {
            Self::send_message_locked(
                stream,
                command,
                request_data,
                &self.timeout_config,
                self.command_has_answer,
                &mut self.buffer,
            )
            .await?;
            let response = Self::receive_message_locked(
                stream,
                &self.timeout_config,
                self.command_has_answer,
                self.max_buffer_size,
                &mut self.buffer,
            )
            .await?;
            Ok(response)
        }
        .await;

        // Update last activity timestamp on success
        if let Err(e) = &result {
            error!("Message exchange failed, dropping connection: {e}");
            *stream_guard = None;
            // Stop ping task since connection is lost
            self.ping_state.stop_ping_task().await;
        } else {
            self.ping_state.touch_activity().await;
        }

        result
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

impl Drop for ClientTLS {
    fn drop(&mut self) {
        // Abort the ping task if it's running
        if let Ok(mut task_handle) = self.ping_state.task_handle().try_lock()
            && let Some(handle) = task_handle.take()
        {
            handle.abort();
        }
        // Signal stop to the ping task
        if let Ok(mut should_stop) = self.ping_state.should_stop().try_lock() {
            *should_stop = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol_structs::Command;
    use crate::request_header::RequestHeader;

    #[test]
    fn ping_header_codec() {
        let req = RequestHeader::new(Command::Ping.into(), 0);
        let mut buf = Vec::new();
        req.encode(&mut buf).unwrap();
        assert_eq!(buf.len(), 8);
        assert_eq!(buf, [0, 0, 0, 0, 0, 0, 0, 0]);

        let mut slice = buf.as_slice();
        let decoded = RequestHeader::decode(&mut slice).unwrap();
        assert_eq!(decoded.command, Command::Ping.into());
        assert_eq!(decoded.data_size, 0);
    }
}
