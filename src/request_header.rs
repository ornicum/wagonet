use crate::{Error, Result};

#[derive(Debug)]
pub struct RequestHeader {
    pub command: u32,
    pub data_size: u32,
}

impl RequestHeader {
    pub const REQUEST_HEADER_SIZE: usize = 8;

    pub fn new(command: u32, data_size: u32) -> Self {
        Self { command, data_size }
    }

    pub fn encoded_len() -> usize {
        Self::REQUEST_HEADER_SIZE
    }

    pub fn encode(&self, buf: &mut Vec<u8>) -> Result<()> {
        buf.extend_from_slice(&self.command.to_be_bytes());
        buf.extend_from_slice(&self.data_size.to_be_bytes());
        Ok(())
    }

    pub fn decode(buf: &mut &[u8], max_data_size: usize) -> Result<Self> {
        if buf.len() < Self::REQUEST_HEADER_SIZE {
            return Err(Error::Protocol(
                "Not enough bytes to decode RequestHeader".to_string(),
            ));
        }

        let mut cmd_bytes = [0u8; 4];
        cmd_bytes.copy_from_slice(&buf[0..4]);
        let command = u32::from_be_bytes(cmd_bytes);

        let mut size_bytes = [0u8; 4];
        size_bytes.copy_from_slice(&buf[4..8]);
        let data_size = u32::from_be_bytes(size_bytes);

        if data_size as usize > max_data_size {
            return Err(Error::BufferOverflow {
                expected: data_size as usize,
                limit: max_data_size,
            });
        }

        *buf = &buf[Self::REQUEST_HEADER_SIZE..];

        Ok(Self { command, data_size })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_header() {
        let header = RequestHeader::new(1, 1);
        let mut buf = Vec::with_capacity(RequestHeader::encoded_len());
        header.encode(&mut buf).unwrap();
        assert_eq!(RequestHeader::encoded_len(), buf.len());
        let header2 = RequestHeader::decode(&mut buf.as_slice(), 1024).unwrap();
        assert_eq!(header.command, header2.command);
        assert_eq!(header.data_size, header2.data_size);
    }

    #[test]
    fn test_request_header_decode_oversized() {
        let header = RequestHeader::new(1, 1000);
        let mut buf = Vec::with_capacity(RequestHeader::encoded_len());
        header.encode(&mut buf).unwrap();
        let result = RequestHeader::decode(&mut buf.as_slice(), 100);
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::BufferOverflow { expected, limit } => {
                assert_eq!(expected, 1000);
                assert_eq!(limit, 100);
            }
            _ => panic!("Expected BufferOverflow error"),
        }
    }
}
