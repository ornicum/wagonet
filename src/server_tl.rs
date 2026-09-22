use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
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
    reader: ReadHalf<&'a mut tokio::net::TcpStream>,
    writer: WriteHalf<&'a mut tokio::net::TcpStream>,
    buffer: Vec<u8>,
    max_buffer_size: usize,
    timeout_config: TimeoutConfig,
}

impl<'a> ServerTL<'a> {
    /// Create a new server handler from split stream halves.
    /// The caller is responsible for accepting the connection and splitting the stream.
    pub fn new(
        reader: ReadHalf<&'a mut tokio::net::TcpStream>,
        writer: WriteHalf<&'a mut tokio::net::TcpStream>,
    ) -> Self {
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
    /// Returns `(command, data_size)` where `data_size` is the payload size in bytes.
    pub async fn read_command(&mut self) -> Result<(u32, usize)> {
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
        if data_size > self.max_buffer_size {
            self.send_response_header(7, 0).await?;
            error!("Request rejected: data size {data_size} exceeds MAX_BUFFER_SIZE");
            return Err("Data size exceeds maximum allowed buffer size".into());
        }
        self.send_response_header(1, 0).await?;
        Ok((req_header.command, req_header.data_size as usize))
    }

    async fn send_response_header(&mut self, status: u8, data_size: u32) -> Result<()> {
        let is_default = data_size == 0;
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
