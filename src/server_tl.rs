use crate::protocol_structs::Command;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{ReadHalf, WriteHalf};
use tracing::error;

use crate::Result;
use crate::common::DEFAULT_MAX_BUFFER_SIZE;
use crate::request_header::RequestHeader;
use crate::response_header::ResponseHeader;
use crate::timeout_config::TimeoutConfig;
/// Plain TCP server handler for a single connection.
///
/// Processes incoming requests using the custom binary protocol:
/// - Reads command header (command + data_size)
/// - Receives request data
/// - Sends response header + optional response data
///
/// Created per-connection from a split `TcpStream` (reader/writer halves).
/// Does not manage the connection lifecycle - the caller handles `accept()` and spawning.
#[derive(Debug)]
pub struct ServerTL<'a> {
    reader: ReadHalf<'a>,
    writer: WriteHalf<'a>,
    buffer: Vec<u8>,
    max_buffer_size: usize,
    timeout_config: TimeoutConfig,
}

impl<'a> ServerTL<'a> {
    /// Create a new server handler from split stream halves.
    /// The caller is responsible for accepting the connection and splitting the stream.
    pub fn new(reader: ReadHalf<'a>, writer: WriteHalf<'a>) -> Self {
        Self {
            reader,
            writer,
            buffer: Vec::with_capacity(DEFAULT_MAX_BUFFER_SIZE),
            max_buffer_size: DEFAULT_MAX_BUFFER_SIZE,
            timeout_config: TimeoutConfig::default(),
        }
    }

    /// Set maximum buffer size for incoming request data (default: 10 MB).
    /// Values <= 0 reset to default.
    pub fn set_max_buffer_size(&mut self, max_buffer_size: usize) {
        self.max_buffer_size = if max_buffer_size > 0 {
            max_buffer_size
        } else {
            DEFAULT_MAX_BUFFER_SIZE
        };
        self.buffer.reserve(max_buffer_size);
    }

    /// Set timeout configuration (read_header, read_data, write).
    pub fn set_timeout_config(&mut self, timeout_config: TimeoutConfig) {
        self.timeout_config = timeout_config;
    }

    /// Read a command header from the client.
    /// Returns `Some((command, data_size))` for regular commands where `data_size` is the payload size.
    /// For Ping command (command=0, data_size=0), sends a 1-byte OK response and returns `Ok(None)`.
    /// The caller can use `if let Some((cmd, sz)) = server.read_command().await?` to handle regular commands.
    pub async fn read_command(&mut self) -> Result<Option<(u32, usize)>> {
        self.buffer.resize(RequestHeader::encoded_len(), 0);

        match tokio::time::timeout(
            self.timeout_config.read_header,
            self.reader.read_exact(&mut self.buffer),
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
            return Ok(None);
        }

        if data_size > self.max_buffer_size {
            self.send_response_header(7, 0).await?;
            error!("Request rejected: data size {data_size} exceeds MAX_BUFFER_SIZE");
            return Err("Data size exceeds maximum allowed buffer size".into());
        }
        self.send_response_header(1, 0).await?;
        Ok(Some((req_header.command, req_header.data_size as usize)))
    }

    async fn send_response_header(&mut self, status: u8, data_size: u32) -> Result<()> {
        let is_default = data_size == 0;
        self.buffer.clear();
        let res_header = ResponseHeader::new(status, data_size);
        res_header.encode(&mut self.buffer, is_default)?;

        match tokio::time::timeout(
            self.timeout_config.write,
            self.writer.write_all(&self.buffer),
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

        self.writer.flush().await?;
        self.buffer.clear();
        Ok(())
    }

    async fn receive_response_header(&mut self, is_default: bool) -> Result<ResponseHeader> {
        let result_buf_len = ResponseHeader::encoded_len(is_default, true);
        self.buffer.resize(result_buf_len, 0);

        match tokio::time::timeout(
            self.timeout_config.read_header,
            self.reader.read_exact(&mut self.buffer),
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

    /// Read request payload data from the client.
    /// `buf_size` must match the `data_size` returned by `read_command()`.
    pub async fn receive_data(&mut self, buf_size: usize) -> Result<Vec<u8>> {
        if buf_size > self.max_buffer_size {
            return Err("Invalid data size".into());
        }
        self.buffer.resize(buf_size, 0);

        match tokio::time::timeout(
            self.timeout_config.read_data,
            self.reader.read_exact(&mut self.buffer),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                error!("Data receive timeout");
                return Err("Timeout waiting for data".into());
            }
        }

        let res = self.buffer.to_vec();
        self.buffer.clear();
        Ok(res)
    }

    /// Send response to the client.
    /// Sends response header (status + data_size), waits for client ACK,
    /// then sends optional payload data.
    pub async fn send_data(&mut self, status: u8, buf: Option<&[u8]>) -> Result<()> {
        let data_size = if let Some(buf) = buf { buf.len() } else { 0 };
        self.send_response_header(status, data_size as u32).await?;
        let res_header = self.receive_response_header(true).await?;
        if res_header.status != 1 {
            return Err("Receiving response status is not OK".into());
        }
        if let Some(buf) = buf {
            match tokio::time::timeout(self.timeout_config.write, self.writer.write_all(buf)).await
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
        self.writer.flush().await?;
        self.buffer.clear();
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_header::RequestHeader;
    use crate::response_header::ResponseHeader;
    use crate::timeout_config::TimeoutConfig;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn server_tl_ping_no_payload() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.split();
            let mut server = ServerTL::new(reader, writer);
            server.set_timeout_config(TimeoutConfig {
                read_header: Duration::from_millis(500),
                ..Default::default()
            });

            let result = server.read_command().await.unwrap();
            assert!(result.is_none(), "Ping should return None");
        });

        // Send ping from client side
        let mut client_stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request_header = RequestHeader::new(0, 0);
        let mut buf = Vec::new();
        request_header.encode(&mut buf).unwrap();
        client_stream.write_all(&buf).await.unwrap();
        client_stream.flush().await.unwrap();

        // Read response (1 byte) - this is the request ACK from read_command
        let mut response = [0u8; 1];
        client_stream.read_exact(&mut response).await.unwrap();
        assert_eq!(response[0], 1); // OK status

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn server_tl_regular_command() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (reader, writer) = stream.split();
            let mut server = ServerTL::new(reader, writer);
            server.set_timeout_config(TimeoutConfig {
                read_header: Duration::from_millis(500),
                ..Default::default()
            });

            let result = server.read_command().await.unwrap();
            assert!(result.is_some(), "Regular command should return Some");
            let (cmd, sz) = result.unwrap();
            assert_eq!(cmd, 42);
            assert_eq!(sz, 5);

            // Receive payload
            let _data = server.receive_data(sz).await.unwrap();

            // Send response
            server.send_data(1, Some(b"OK")).await.unwrap();
        });

        // Send regular command from client side
        let mut client_stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request_header = RequestHeader::new(42, 5);
        let mut buf = Vec::new();
        request_header.encode(&mut buf).unwrap();
        client_stream.write_all(&buf).await.unwrap();
        client_stream.flush().await.unwrap();

        // Read request ACK (1 byte) - sent by read_command
        let mut request_ack = [0u8; 1];
        client_stream.read_exact(&mut request_ack).await.unwrap();
        assert_eq!(request_ack[0], 1); // OK status

        // Send payload
        client_stream.write_all(b"hello").await.unwrap();
        client_stream.flush().await.unwrap();

        // Read response header (5 bytes: status + data_size since data_size > 0)
        let mut resp_header_buf = [0u8; 5];
        client_stream.read_exact(&mut resp_header_buf).await.unwrap();
        let mut slice = &resp_header_buf[..];
        // is_default = false because data_size > 0
        let resp_header = ResponseHeader::decode(&mut slice, false).unwrap();
        assert_eq!(resp_header.status, 1); // OK status
        assert_eq!(resp_header.data_size, 2);

        // Send ACK for response header
        let ack = ResponseHeader::new(1, 0);
        let mut ack_buf = Vec::new();
        ack.encode(&mut ack_buf, true).unwrap();
        client_stream.write_all(&ack_buf).await.unwrap();
        client_stream.flush().await.unwrap();

        // Read response payload
        let mut payload = vec![0u8; resp_header.data_size as usize];
        client_stream.read_exact(&mut payload).await.unwrap();
        assert_eq!(payload, b"OK");

        server_handle.await.unwrap();
    }
}
