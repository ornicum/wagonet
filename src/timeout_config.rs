use std::time::Duration;
use crate::{Error, Result};

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
    /// Returns an error if the invariant is violated.
    pub fn validate(&self) -> Result<()> {
        if self.ping_interval >= self.read_header {
            return Err(Error::Config(format!(
                "TimeoutConfig invariant violated: ping_interval ({:?}) >= read_header ({:?}). \
                Pings may not prevent server-side connection timeout.",
                self.ping_interval, self.read_header
            )));
        }
        Ok(())
    }

    /// Set ping_interval with automatic clamping to maintain invariant.
    /// If interval >= read_header, it will be clamped to read_header - 1s
    /// (or read_header / 2 if read_header is small).
    pub fn with_ping_interval(mut self, interval: Duration) -> Self {
        self.ping_interval = interval;
        // Clamp to maintain ping_interval < read_header
        if self.ping_interval >= self.read_header {
            if self.read_header > Duration::ZERO {
                let clamped = self.read_header.saturating_sub(Duration::from_secs(1));
                self.ping_interval = if clamped == Duration::ZERO
                    && self.read_header >= Duration::from_millis(200)
                {
                    self.read_header / 2
                } else {
                    clamped
                };
            } else {
                self.ping_interval = Duration::ZERO;
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn with_ping_interval_clamps_when_interval_ge_read_header() {
        // Test that ping_interval is clamped when >= read_header
        let config = TimeoutConfig::default()
            .with_ping_interval(Duration::from_secs(60)); // read_header default is 60s
        
        // Should be clamped to read_header - 1s = 59s
        assert_eq!(config.ping_interval, Duration::from_secs(59));
    }

    #[test]
    fn with_ping_interval_clamps_to_half_when_read_header_small() {
        // Test that ping_interval is clamped to read_header / 2 when read_header < 2s
        let config = TimeoutConfig {
            read_header: Duration::from_millis(500),
            ..Default::default()
        }.with_ping_interval(Duration::from_secs(10)); // 10s > 500ms
        
        // Should be clamped to 500ms / 2 = 250ms
        assert_eq!(config.ping_interval, Duration::from_millis(250));
    }

    #[test]
    fn with_ping_interval_does_not_change_when_valid() {
        // Test that valid ping_interval is not changed
        let config = TimeoutConfig::default()
            .with_ping_interval(Duration::from_secs(30)); // 30s < 60s
        
        assert_eq!(config.ping_interval, Duration::from_secs(30));
    }

    #[test]
    fn validate_returns_ok_when_valid() {
        let config = TimeoutConfig::default();
        let result = config.validate();
        assert!(result.is_ok());
    }

    #[test]
    fn validate_returns_error_when_invalid() {
        let config = TimeoutConfig {
            read_header: Duration::from_secs(10),
            ping_interval: Duration::from_secs(20),
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
        
        // Check error message
        let err = result.unwrap_err();
        let err_str = err.to_string();
        assert!(err_str.contains("ping_interval"));
        assert!(err_str.contains("read_header"));
    }

    #[test]
    fn validate_returns_error_when_equal() {
        // When ping_interval == read_header, it's invalid
        let config = TimeoutConfig {
            read_header: Duration::from_secs(10),
            ping_interval: Duration::from_secs(10),
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
    }
}
