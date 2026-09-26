use crate::protocol_structs::Command;
use crate::timeout_config::TimeoutConfig;
use crate::{Error, Result};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio::time::sleep;

/// Trait for stream operations needed by the ping protocol.
/// Both ClientTL and ClientTLS implement this (or adapt to it).
#[async_trait::async_trait]
pub trait PingStream: Send + Sync {
    /// Send a ping request header (Command::Ping, data_size=0).
    async fn send_ping_request(&mut self) -> Result<()>;

    /// Receive and validate ping response header (status=Ok, data_size=0).
    async fn receive_ping_response(&mut self) -> Result<()>;

    /// Check if the stream is connected.
    fn is_connected(&self) -> bool;
}

/// Configuration for the ping background task.
#[derive(Debug, Clone)]
pub struct PingConfig {
    /// Interval between ping checks.
    pub interval: Duration,
    /// If true, the ping task will run. If false, pings are disabled.
    pub enabled: bool,
}

impl Default for PingConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
            enabled: true,
        }
    }
}

/// Shared state for the ping background task.
#[derive(Debug)]
pub struct PingState {
    /// Time of last activity (any send/receive).
    last_activity: Arc<Mutex<Instant>>,
    /// Mutex to prevent ping from racing with regular requests.
    in_flight: Arc<Mutex<()>>,
    /// Handle to the background ping task (None if not running).
    task_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Whether the ping task should stop.
    should_stop: Arc<Mutex<bool>>,
    /// Ping configuration (interval, enabled).
    config: Arc<Mutex<PingConfig>>,
    /// Timeout configuration for ping (read_header for response, etc.).
    timeout_config: Arc<Mutex<TimeoutConfig>>,
}

impl PingState {
    pub fn new(config: PingConfig, timeout_config: TimeoutConfig) -> Arc<Self> {
        Arc::new(Self {
            last_activity: Arc::new(Mutex::new(Instant::now())),
            in_flight: Arc::new(Mutex::new(())),
            task_handle: Mutex::new(None),
            should_stop: Arc::new(Mutex::new(false)),
            config: Arc::new(Mutex::new(config)),
            timeout_config: Arc::new(Mutex::new(timeout_config)),
        })
    }

    /// Update the last activity timestamp to now.
    pub async fn touch_activity(&self) {
        let mut guard = self.last_activity.lock().await;
        *guard = Instant::now();
    }

    /// Get the time since last activity.
    pub async fn elapsed_since_activity(&self) -> Duration {
        let guard = self.last_activity.lock().await;
        guard.elapsed()
    }

    /// Get the ping interval.
    pub async fn interval(&self) -> Duration {
        self.config.lock().await.interval
    }

    /// Check if ping is enabled.
    pub async fn is_enabled(&self) -> bool {
        self.config.lock().await.enabled
    }

    /// Acquire the in-flight lock for a regular request.
    /// Returns a guard that releases the lock on drop.
    pub async fn acquire_in_flight(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.in_flight.lock().await
    }

    /// Try to acquire the in-flight lock for a ping.
    /// Returns None if a request is in flight (ping should wait).
    pub async fn try_acquire_in_flight_for_ping(&self) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        self.in_flight.try_lock().ok()
    }

    /// Get a reference to the last activity timestamp mutex.
    /// Allows reading the last activity time without modifying it.
    pub fn last_activity(&self) -> Arc<Mutex<Instant>> {
        self.last_activity.clone()
    }

    /// Get a reference to the in-flight mutex.
    /// Allows acquiring the lock for coordinating with ping.
    pub fn in_flight(&self) -> Arc<Mutex<()>> {
        self.in_flight.clone()
    }

    /// Get a reference to the ping configuration mutex.
    /// Allows reading/modifying ping configuration.
    pub fn config(&self) -> Arc<Mutex<PingConfig>> {
        self.config.clone()
    }

    /// Get a reference to the timeout configuration mutex.
    /// Allows reading/modifying timeout configuration.
    pub fn timeout_config(&self) -> Arc<Mutex<TimeoutConfig>> {
        self.timeout_config.clone()
    }

    /// Get a reference to the should_stop flag mutex.
    /// Allows signaling the ping task to stop.
    pub fn should_stop(&self) -> Arc<Mutex<bool>> {
        self.should_stop.clone()
    }

    /// Get a reference to the task handle mutex.
    /// Allows managing the background task handle.
    pub fn task_handle(&self) -> &Mutex<Option<tokio::task::JoinHandle<()>>> {
        &self.task_handle
    }

    /// Set the should_stop flag to signal the ping task to stop.
    pub async fn set_should_stop(&self, val: bool) {
        *self.should_stop.lock().await = val;
    }

    /// Take the task handle, leaving None in its place.
    pub async fn take_task_handle(&self) -> Option<tokio::task::JoinHandle<()>> {
        let mut guard = self.task_handle.lock().await;
        guard.take()
    }

    /// Start the background ping task.
    /// `stream` is a reference-counted pointer to the stream implementation.
    pub async fn start_ping_task<S>(self: Arc<Self>, stream: Arc<Mutex<Option<S>>>)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        if !self.is_enabled().await {
            return;
        }

        // Reset should_stop flag for new task
        *self.should_stop.lock().await = false;

        let in_flight = self.in_flight.clone();
        let last_activity = self.last_activity.clone();
        let should_stop = self.should_stop.clone();
        let self_arc = self.clone(); // Clone Arc for use in spawned task
        let handle = tokio::spawn(async move {
            loop {
                // Read interval dynamically each iteration
                let interval = self_arc.interval().await;
                let mut check_interval = interval / 4; // Check 4x per interval
                if check_interval < Duration::from_millis(100) {
                    check_interval = Duration::from_millis(100);
                }
                sleep(check_interval).await;

                // Check if we should stop
                if *should_stop.lock().await {
                    break;
                }

                // Check if enough time has passed since last activity
                let elapsed = {
                    let guard = last_activity.lock().await;
                    guard.elapsed()
                };

                // Re-read interval in case it changed during sleep
                let current_interval = self_arc.interval().await;
                if elapsed < current_interval {
                    continue;
                }

                // Try to acquire the in-flight lock for ping
                let ping_guard = match in_flight.try_lock() {
                    Ok(guard) => guard,
                    Err(_) => {
                        // Another request is in flight, skip this ping cycle
                        continue;
                    }
                };

                // Send ping
                let mut stream_guard = stream.lock().await;
                let stream_ref = match stream_guard.as_mut() {
                    Some(s) => s,
                    None => {
                        // Stream disconnected, stop ping task
                        drop(ping_guard);
                        *should_stop.lock().await = true;
                        break;
                    }
                };

                // Read read_header dynamically each ping
                let read_header = self_arc.timeout_config.lock().await.read_header;
                let ping_result = async {
                    send_ping_request_impl(stream_ref).await?;
                    receive_ping_response_impl(stream_ref, read_header).await
                }
                .await;

                if let Err(e) = ping_result {
                    tracing::error!("Ping failed: {}. Marking stream as disconnected.", e);
                    *stream_guard = None;
                    *should_stop.lock().await = true;
                    break;
                }

                // Ping successful, update last activity
                let mut activity_guard = last_activity.lock().await;
                *activity_guard = Instant::now();
            }
        });

        let mut task_handle = self.task_handle.lock().await;
        *task_handle = Some(handle);
    }

    /// Stop the background ping task.
    ///
    /// Signals the ping task to stop and aborts the task handle.
    /// Does not wait for the task to finish - the task will exit
    /// when it next checks the should_stop flag.
    pub fn stop_ping_task(&self) {
        if let Ok(mut should_stop) = self.should_stop.try_lock() {
            *should_stop = true;
        }
        if let Ok(mut task_handle) = self.task_handle.try_lock()
            && let Some(handle) = task_handle.take()
        {
            handle.abort();
        }
    }
}

/// Send a ping request using the generic stream interface.
/// This is a helper that can be used by implementations.
pub async fn send_ping_request_impl<W>(writer: &mut W) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin + Send,
{
    use crate::request_header::RequestHeader;

    let request_header = RequestHeader::new(Command::Ping.into(), 0);
    let mut buf = Vec::with_capacity(RequestHeader::encoded_len());
    request_header.encode(&mut buf)?;

    // Note: timeout handling is done by the caller
    writer.write_all(&buf).await?;
    writer.flush().await?;
    Ok(())
}

/// Receive and validate a ping response.
/// Supports both new server (1-byte response: status=1) and old server (5-byte response header with status=1, data_size=0).
pub async fn receive_ping_response_impl<R>(
    reader: &mut R,
    read_header_timeout: Duration,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send,
{
    // First, read 1 byte (status) with the full timeout
    let mut status_buf = [0u8; 1];
    tokio::time::timeout(read_header_timeout, reader.read_exact(&mut status_buf))
        .await
        .map_err(|_| Error::Timeout("Ping response header timeout".to_string()))?
        .map_err(Error::from)?;

    let status = status_buf[0];
    if status != 1 {
        return Err(Error::ResponseError { status });
    }

    // Status is OK. Now try to read 4 more bytes (data_size) with a very short timeout.
    // New server sends only 1 byte (status=1, data_size=0 encoded as default).
    // Old server sends 5 bytes total (status=1 + data_size=4 bytes).
    // We try to read 4 more bytes with a very short timeout (10ms).
    // If they arrive, it's an old server response; if timeout, it's a new server 1-byte response.
    let mut size_buf = [0u8; 4];
    match tokio::time::timeout(Duration::from_millis(10), reader.read_exact(&mut size_buf)).await {
        Ok(Ok(_)) => {
            // Got 4 more bytes - old server response
            let data_size = u32::from_be_bytes(size_buf);
            if data_size > 0 {
                // Read and discard payload (shouldn't happen for ping, but handle gracefully)
                let mut payload = vec![0u8; data_size as usize];
                reader.read_exact(&mut payload).await.map_err(Error::from)?;
            }
            // data_size == 0: no payload
            Ok(())
        }
        Ok(Err(e)) => Err(e.into()), // IO error
        Err(_) => {
            // Timeout - no more bytes available, assume new server 1-byte response
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol_structs::Command;
    use crate::request_header::RequestHeader;
    use crate::timeout_config::TimeoutConfig;
    use super::{PingState, PingConfig};

    #[test]
    fn ping_header_codec() {
        // Ping request: command=0, data_size=0
        let req = RequestHeader::new(Command::Ping.into(), 0);
        let mut buf = Vec::new();
        req.encode(&mut buf).unwrap();
        assert_eq!(buf.len(), 8);
        assert_eq!(buf, [0, 0, 0, 0, 0, 0, 0, 0]);

        let mut slice = buf.as_slice();
        let decoded = RequestHeader::decode(&mut slice, 1024).unwrap();
        assert_eq!(decoded.command, Command::Ping.into());
        assert_eq!(decoded.data_size, 0);
    }

    #[tokio::test]
    async fn pingstate_set_should_stop_and_take_task_handle() {
        let ping_state = PingState::new(PingConfig::default(), TimeoutConfig::default());
        
        // Test set_should_stop
        ping_state.set_should_stop(true).await;
        assert!(*ping_state.should_stop().lock().await);
        
        ping_state.set_should_stop(false).await;
        assert!(!*ping_state.should_stop().lock().await);
        
        // Test take_task_handle (should return None initially)
        let handle = ping_state.take_task_handle().await;
        assert!(handle.is_none());
    }
}
