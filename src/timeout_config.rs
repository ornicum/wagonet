use std::time::Duration;
use tracing::warn;

#[derive(Debug, Clone)]
pub struct TimeoutConfig {
    pub connect: Duration,
    pub read_header: Duration,
    pub read_data: Duration,
    pub write: Duration,
    pub keep_alive: Option<KeepAliveConfig>,
    /// Interval for sending ping requests to keep the connection alive.
    /// Default: 30 seconds. Must be less than read_header to be effective.
    pub ping_interval: Duration,
}

#[derive(Debug, Clone)]
pub struct KeepAliveConfig {
    pub time: Duration,
    pub interval: Duration,
}

impl Default for KeepAliveConfig {
    fn default() -> Self {
        Self {
            time: Duration::from_secs(30),
            interval: Duration::from_secs(10),
        }
    }
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            read_header: Duration::from_secs(60),
            read_data: Duration::from_secs(60),
            write: Duration::from_secs(60),
            keep_alive: None,
            ping_interval: Duration::from_secs(30),
        }
    }
}

impl TimeoutConfig {
    /// Validate that ping_interval < read_header.
    /// Logs a warning if the invariant is violated.
    pub fn validate(&self) {
        if self.ping_interval >= self.read_header {
            warn!(
                "TimeoutConfig invariant violated: ping_interval ({:?}) >= read_header ({:?}). \
                Pings may not prevent server-side connection timeout.",
                self.ping_interval, self.read_header
            );
        }
    }
}
