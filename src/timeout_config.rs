use std::time::Duration;

#[derive(Debug, Clone)]
pub struct TimeoutConfig {
    pub connect: Duration,
    pub read_header: Duration,
    pub read_data: Duration,
    pub write: Duration,
    pub keep_alive: Option<KeepAliveConfig>,
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
        }
    }
}
