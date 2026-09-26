# wagonet

High-performance async TCP/TLS transport library with custom binary protocol.

[![Crates.io](https://img.shields.io/crates/v/wagonet.svg)](https://crates.io/crates/wagonet)
[![Documentation](https://docs.rs/wagonet/badge.svg)](https://docs.rs/wagonet)
[![License](https://img.shields.io/crates/l/wagonet.svg)](https://github.com/ornicum/wagonet#license)
[![Build Status](https://github.com/ornicum/wagonet/workflows/CI/badge.svg)](https://github.com/ornicum/wagonet/actions)

## Features

- **Custom Binary Protocol**: 1-byte header for empty payloads, 5-byte header (u8 status + u32 data_size) for data
- **TCP & TLS Support**: Native TLS via `native-tls`/`tokio-native-tls`, plain TCP via `tokio::net`
- **Configurable Timeouts**: Separate timeouts for connect, read header, read data, write
- **TCP Keep-Alive**: Configurable idle time and probe interval via `socket2`
- **Connection Reuse**: Keep-alive with bounded retry (1 retry) and health-aware reconnection
- **Secure TLS Defaults**: Certificate validation enabled by default
- **No-Answer Mode**: Fire-and-forget commands
- **Buffer Protection**: Configurable max buffer size (10 MB default)
- **Async/Await**: Built on `tokio` with full async/await support

## Quick Start

Add to `Cargo.toml`:

```toml
[dependencies]
wagonet = "0.5"
```

### Plain TCP Client

```rust
use wagonet::{ClientTL, TimeoutConfig, KeepAliveConfig};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = ClientTL::new("127.0.0.1:8080".to_string());
    
    // Enable keep-alive with custom timing
    client.set_keep_alive(true);
    client.set_timeout_config(TimeoutConfig {
        keep_alive: Some(KeepAliveConfig {
            time: Duration::from_secs(60),    // idle before first probe
            interval: Duration::from_secs(15), // interval between probes
        }),
        ..Default::default()
    });

    // Multiple requests reuse the same connection
    let response = client.handle_message(1, b"hello").await?;
    println!("Response: {:?}", String::from_utf8_lossy(&response));

    let response2 = client.handle_message(2, b"world").await?;
    println!("Response 2: {:?}", String::from_utf8_lossy(&response2));

    Ok(())
}
```

### TLS Client

```rust
use wagonet::{ClientTLS, TimeoutConfig};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = ClientTLS::new("example.org:443".to_string());
    
    // TLS certificate validation is ON by default
    client.set_domain("example.org".to_string());
    
    // For self-signed certs in development only:
    // client.set_accept_invalid_certs(true);

    let response = client.handle_message(1, b"hello").await?;
    println!("Response: {:?}", response);

    Ok(())
}
```

### Server

```rust
use wagonet::{ServerTL, ServerTLS};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:8080").await?;
    
    loop {
        let (stream, addr) = listener.accept().await?;
        println!("Client connected: {}", addr);
        
        let (reader, writer) = stream.split();
        let mut server = ServerTL::new(reader, writer);
        
        tokio::spawn(async move {
            loop {
                match server.read_command().await {
                    Ok(Some((cmd, size))) => {
                        let data = server.receive_data(size).await?;
                        println!("Received cmd={}, size={}, data={:?}", cmd, size, data);
                        
                        // Echo back
                        server.send_data(0, Some(&data)).await?;
                    }
                    Ok(None) => {
                        // Ping handled automatically, ACK already sent
                        continue;
                    }
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        break;
                    }
                }
            }
        });
```

## Configuration

### TimeoutConfig

```rust
pub struct TimeoutConfig {
    pub connect: Duration,        // Default: 10s
    pub read_header: Duration,    // Default: 60s
    pub read_data: Duration,      // Default: 60s
    pub write: Duration,          // Default: 60s
    pub keep_alive: Option<KeepAliveConfig>, // None = OS defaults
}
```

### KeepAliveConfig

```rust
pub struct KeepAliveConfig {
    pub time: Duration,      // TCP_KEEPIDLE: idle time before first probe (default: 30s)
    pub interval: Duration,  // TCP_KEEPINTVL: interval between probes (default: 10s)
}
```

## Protocol

### Request Header (Client → Server)

| Field | Size | Description |
|-------|------|-------------|
| command | u32 | Command identifier |
| data_size | u32 | Payload size in bytes |

### Response Header (Server → Client)

| Field | Size | Description |
|-------|------|-------------|
| status | u8 | 0 = OK, 1-255 = error codes |
| data_size | u32 | Payload size in bytes |

### Empty Payload Optimization

When `data_size == 0`, the header is only 1 byte (just the status byte).

## API Reference

### ClientTL (Plain TCP)

```rust
impl ClientTL {
    pub fn new(address: String) -> Self
    pub fn set_max_buffer_size(&mut self, buffer_size: usize)
    pub fn set_command_has_answer(&mut self, command_has_answer: bool)
    pub fn set_timeout_config(&mut self, timeout_config: TimeoutConfig)
    pub fn set_keep_alive(&mut self, enabled: bool)
    pub async fn connect(&mut self) -> Result<(), Box<dyn Error + Send + Sync>>
    pub async fn disconnect(&mut self) -> Result<(), Box<dyn Error + Send + Sync>>
    pub async fn handle_message(&mut self, command: u32, request_data: &[u8]) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>>
    pub async fn handle_message_with_no_answer(&mut self, command: u32, request_data: &[u8]) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>>
}
```

### ClientTLS (TLS)

```rust
impl ClientTLS {
    pub fn new(address: String) -> Self
    pub fn set_max_buffer_size(&mut self, buffer_size: usize)
    pub fn set_command_has_answer(&mut self, command_has_answer: bool)
    pub fn set_accept_invalid_certs(&mut self, accept_invalid_certs: bool)
    pub fn set_domain(&mut self, domain: String)
    pub fn set_timeout_config(&mut self, timeout_config: TimeoutConfig)
    pub fn set_keep_alive(&mut self, enabled: bool)
    pub async fn connect(&mut self) -> Result<(), Box<dyn Error + Send + Sync>>
    pub async fn disconnect(&mut self) -> Result<(), Box<dyn Error + Send + Sync>>
    pub async fn handle_message(&mut self, command: u32, request_data: &[u8]) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>>
    pub async fn handle_message_with_no_answer(&mut self, command: u32, request_data: &[u8]) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>>
}
```

### ServerTL / ServerTLS

```rust
impl<'a> ServerTL<'a> {
    pub fn new(reader: ReadHalf<'a>, writer: WriteHalf<'a>) -> Self
    pub fn set_max_buffer_size(&mut self, max_buffer_size: usize)
    pub fn set_timeout_config(&mut self, timeout_config: TimeoutConfig)
    pub async fn read_command(&mut self) -> Result<Option<(u32, usize)>>
    pub async fn receive_data(&mut self, buf_size: usize) -> Result<Vec<u8>>
    pub async fn send_data(&mut self, status: u8, buf: Option<&[u8]>) -> Result<()>
}
```

## Architecture

See [docs/architecture.md](docs/architecture.md) for:
- Component diagrams
- Connection lifecycle state machine
- Request/response sequence diagrams
- Error handling flows
- Security analysis

## Examples

See [examples/](examples/) directory:
- `simple_client.rs` - Basic TCP client
- `simple_server.rs` - Basic TCP server
- `tls_client.rs` - TLS client with certificate validation
- `tls_server.rs` - TLS server
- `no_answer_mode.rs` - Fire-and-forget commands

## Security

- TLS certificate validation **enabled by default** (`accept_invalid_certs: false`)
- No secrets in logs (only fixed strings and error codes)
- Bounded retries (no infinite loops)
- No sensitive data in error messages
- No secrets in codebase (verified by security audit)
- Connection cleanup on error paths

## Testing

```bash
cargo test
# 6 tests passing:
# - test_tcp_transport_success
# - test_tcp_transport_buffer_overflow
# - test_tcp_transport_no_answer
# - test_tls_transport_success
# - test_request_header
# - test_response_header
```

## License

Licensed under either of

- MIT License ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)

at your option.
## Contributing
