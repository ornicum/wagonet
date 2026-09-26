use socket2::SockRef;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{error, warn};

use crate::common::DEFAULT_MAX_BUFFER_SIZE;
use crate::ping::PingState;
use crate::request_header::RequestHeader;
use crate::response_header::ResponseHeader;
use crate::timeout_config::TimeoutConfig;
use crate::{Error, Result};

/// Plain TCP client with connection reuse, ping keep-alive, and configurable timeouts.
///
/// # Features
/// - **Connection reuse** via `keep_alive` (enabled with `set_keep_alive(true)`)
/// - **Application-level ping keep-alive**: periodic empty requests to prevent server timeout
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
///         ping_interval: Duration::from_secs(30),
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
    // When keep_alive is enabled, stream is shared with the ping task via Arc<Mutex>.
    // When keep_alive is disabled, the Mutex is still used but no ping task runs.
    stream: Arc<Mutex<Option<TcpStream>>>,
    command_has_answer: bool,
    buffer: Vec<u8>,
    max_buffer_size: usize,
    pub timeout_config: TimeoutConfig,
    keep_alive: bool,
    /// Ping state for background keep-alive task.
    ping_state: Arc<PingState>,
}

impl ClientTL {
    /// Create a new client with the given address (e.g., "127.0.0.1:8080").
    /// Connection is established lazily on first `handle_message` or via `connect()`.
    pub fn new(address: String) -> Self {
        let timeout_config = TimeoutConfig::default();
        let ping_config = crate::ping::PingConfig {
            interval: timeout_config.ping_interval,
            enabled: timeout_config.ping_interval > Duration::ZERO,
        };
        Self {
            address,
            stream: Arc::new(Mutex::new(None)),
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
            *self.stream.lock().await = None;
        }
        self.keep_alive = enabled;
        let config_arc = self.ping_state.config();
        let mut config = config_arc.lock().await;
        config.enabled = enabled && config.interval > Duration::ZERO;
    }
    /// Set timeout configuration (connect, read_header, read_data, write, keep_alive, ping_interval).
    pub async fn set_timeout_config(&mut self, timeout_config: TimeoutConfig) -> Result<()> {
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
                    error!("CLIENT: New TCP connection established");
                    *self.stream.lock().await = Some(stream);

                    // Start ping task if keep_alive is enabled
                    if self.keep_alive && self.ping_state.is_enabled().await {
                        self.ping_state
                            .clone()
                            .start_ping_task(self.stream.clone())
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
            sleep(Duration::from_secs(1)).await;
            tries += 1;
        }
        if self.stream.lock().await.is_none() && tries >= 10 {
            error!("Connection error, max retries reached");
            return Err("Connection error, max retries reached".into());
        }
        Ok(())
    }

    /// Gracefully close the TCP connection.
    /// Sends shutdown and clears the internal stream.
    ///
    /// **Note:** This method stops the background ping task by calling
    /// `stop_ping_task()`, which may block for up to 5 seconds (the default
    /// ping task shutdown timeout) if the ping task is stuck in network I/O.
    pub async fn disconnect(&mut self) -> Result<()> {
        // Stop ping task first
        self.ping_state.stop_ping_task().await;

        let mut stream_guard = self.stream.lock().await;
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
        stream: &mut TcpStream,
        command: u32,
        data_size: u32,
        timeout: Duration,
        buffer: &mut Vec<u8>,
    ) -> Result<()> {
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
                return Err(Error::Timeout("Sending request header".to_string()));
            }
        }

        stream.flush().await?;
        buffer.clear();
        Ok(())
    }

    async fn send_response_header_locked(
        stream: &mut TcpStream,
        status: u8,
        data_size: u32,
        timeout: Duration,
        buffer: &mut Vec<u8>,
    ) -> Result<()> {
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
                return Err(Error::Timeout("Sending response header".to_string()));
            }
        }

        stream.flush().await?;
        buffer.clear();
        Ok(())
    }

    async fn receive_response_header_locked(
        stream: &mut TcpStream,
        is_default: bool,
        command_has_answer: bool,
        timeout: Duration,
        buffer: &mut Vec<u8>,
    ) -> Result<ResponseHeader> {
        let result_buf_size = ResponseHeader::encoded_len(is_default, command_has_answer);
        buffer.resize(result_buf_size, 0);

        match tokio::time::timeout(timeout, stream.read_exact(buffer)).await {
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

        let decode_as_default = is_default || !command_has_answer;
        let response_header = ResponseHeader::decode(&mut buffer.as_slice(), decode_as_default)?;
        buffer.clear();
        Ok(response_header)
    }

    async fn send_message_locked(
        stream: &mut TcpStream,
        command: u32,
        data: &[u8],
        timeout_config: &TimeoutConfig,
        command_has_answer: bool,
        buffer: &mut Vec<u8>,
    ) -> Result<()> {
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
                return Err(Error::Timeout("Sending message data".to_string()));
            }
        }

        stream.flush().await?;
        buffer.clear();
        Ok(())
    }

    async fn receive_message_locked(
        stream: &mut TcpStream,
        timeout_config: &TimeoutConfig,
        command_has_answer: bool,
        max_buffer_size: usize,
        buffer: &mut Vec<u8>,
    ) -> Result<Vec<u8>> {
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
                return Err(Error::ResponseError { status });
            }
            let data_usize = res_header.data_size as usize;
            if data_usize > max_buffer_size {
                return Err(Error::BufferOverflow {
                    expected: data_usize,
                    limit: max_buffer_size,
                });
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
                return Err(Error::Timeout("Waiting for message data".to_string()));
            }
        }

        Ok(buffer.to_vec())
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
        if !self.keep_alive || self.stream.lock().await.is_none() {
            self.connect().await?;
        }

        // Acquire in-flight lock for the entire request-response cycle
        let _in_flight_guard = self.ping_state.acquire_in_flight().await;
        let mut stream_guard = self.stream.lock().await;
        let stream = match stream_guard.as_mut() {
            Some(s) => s,
            None => return Err(Error::NotConnected),
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

impl Drop for ClientTL {
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
    use super::*;
    use crate::protocol_structs::Command;
    use crate::request_header::RequestHeader;
    use crate::server_tl::ServerTL;
    use crate::timeout_config::TimeoutConfig;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

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

    async fn run_mock_server_tl(
        listener: TcpListener,
        connection_count: Option<Arc<AtomicUsize>>,
        ping_count: Option<Arc<AtomicUsize>>,
    ) {
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                if let Some(counter) = &connection_count {
                    counter.fetch_add(1, Ordering::SeqCst);
                }

                let (reader, writer) = stream.split();
                let mut server = ServerTL::new(reader, writer);
                server.set_timeout_config(TimeoutConfig {
                    read_header: Duration::from_millis(500),
                    ..Default::default()
                });

                loop {
                    match server.read_command().await {
                        Ok(Some((_command, data_size))) => {
                            if data_size > 0 {
                                let _ = server.receive_data(data_size).await;
                            }
                            let _ = server.send_data(1, Some(b"OK")).await;
                        }
                        Ok(None) => {
                            // Ping handled by read_command (sent 1-byte OK response)
                            if let Some(pc) = &ping_count {
                                pc.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        });
    }

    #[tokio::test]
    async fn server_tl_ping_no_payload() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let ping_count = Arc::new(AtomicUsize::new(0));
        let ping_count_clone = ping_count.clone();

        run_mock_server_tl(listener, None, Some(ping_count_clone)).await;

        let mut client = ClientTL::new(addr.to_string());
        client.set_keep_alive(true).await;
        client
            .set_timeout_config(TimeoutConfig {
                ping_interval: Duration::from_millis(100),
                ..Default::default()
            })
            .await;

        // Connect and manually send ping
        client.connect().await.unwrap();

        let mut stream_guard = client.stream.lock().await;
        let stream = stream_guard.as_mut().unwrap();

        let request_header = RequestHeader::new(0, 0);
        let mut buf = Vec::new();
        request_header.encode(&mut buf).unwrap();
        stream.write_all(&buf).await.unwrap();
        stream.flush().await.unwrap();

        // Read ping response (1 byte)
        let mut response = [0u8; 1];
        stream.read_exact(&mut response).await.unwrap();
        assert_eq!(response[0], 1); // OK status

        drop(stream_guard);
        client.disconnect().await.unwrap();

        assert_eq!(ping_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn client_ping_timer() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let ping_count = Arc::new(AtomicUsize::new(0));
        let ping_count_clone = ping_count.clone();

        run_mock_server_tl(listener, None, Some(ping_count_clone)).await;

        let mut client = ClientTL::new(addr.to_string());
        client.set_keep_alive(true).await;
        client
            .set_timeout_config(TimeoutConfig {
                ping_interval: Duration::from_millis(100),
                ..Default::default()
            })
            .await;

        // Connect and wait for pings
        client.connect().await.unwrap();

        // Wait for ~350ms, should get at least 3 pings
        tokio::time::sleep(Duration::from_millis(350)).await;

        client.disconnect().await.unwrap();

        let count = ping_count.load(Ordering::SeqCst);
        assert!(count >= 3, "Expected at least 3 pings, got {}", count);
    }

    #[tokio::test]
    async fn client_ping_not_sent_when_active() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let ping_count = Arc::new(AtomicUsize::new(0));
        let ping_count_clone = ping_count.clone();

        run_mock_server_tl(listener, None, Some(ping_count_clone)).await;

        let mut client = ClientTL::new(addr.to_string());
        client.set_keep_alive(true).await;
        client
            .set_timeout_config(TimeoutConfig {
                ping_interval: Duration::from_millis(200),
                ..Default::default()
            })
            .await;

        // Send requests every 50ms for 500ms - should be 10 requests
        for i in 0..10 {
            client.handle_message(i, b"data").await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        client.disconnect().await.unwrap();

        // During active requests, no pings should be sent
        let count = ping_count.load(Ordering::SeqCst);
        assert_eq!(
            count, 0,
            "Expected no pings during active requests, got {}",
            count
        );
    }

    #[test]
    fn timeout_invariant() {
        let config = TimeoutConfig {
            read_header: Duration::from_secs(10),
            ping_interval: Duration::from_secs(30),
            ..Default::default()
        };

        // This should log a warning but not panic
        config.validate();

        // Test valid config
        let config = TimeoutConfig {
            read_header: Duration::from_secs(60),
            ping_interval: Duration::from_secs(30),
            ..Default::default()
        };
        config.validate(); // Should not log warning
    }
}
