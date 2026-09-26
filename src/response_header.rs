use crate::{Error, Result};

#[derive(Debug)]
pub struct ResponseHeader {
    pub status: u8,
    pub data_size: u32,
}

impl ResponseHeader {
    pub const DEFAULT_RESPONSE_HEADER_SIZE: usize = 1;
    pub const RESPONSE_HEADER_SIZE: usize = 5;

    pub fn new(status: u8, data_size: u32) -> Self {
        Self { status, data_size }
    }

    pub fn encoded_len(is_default: bool, command_has_answer: bool) -> usize {
        if is_default {
            Self::DEFAULT_RESPONSE_HEADER_SIZE
        } else {
            if command_has_answer {
                Self::RESPONSE_HEADER_SIZE
            } else {
                Self::DEFAULT_RESPONSE_HEADER_SIZE
            }
        }
    }

    pub fn encode(&self, buf: &mut Vec<u8>, is_default: bool) -> Result<()> {
        buf.push(self.status);
        if !is_default {
            buf.extend_from_slice(&self.data_size.to_be_bytes());
        }
        Ok(())
    }

    pub fn decode(buf: &mut &[u8], is_default: bool, max_data_size: usize) -> Result<Self> {
        if buf.is_empty() {
            return Err(Error::Protocol(
                "Not enough bytes to decode ResponseHeader".to_string(),
            ));
        }

        let status = buf[0];

        if is_default {
            *buf = &buf[Self::DEFAULT_RESPONSE_HEADER_SIZE..];
            Ok(Self {
                status,
                data_size: 0,
            })
        } else {
            if buf.len() < Self::RESPONSE_HEADER_SIZE {
                return Err(Error::Protocol(
                    "Not enough bytes to decode ResponseHeader".to_string(),
                ));
            }
            let mut size_bytes = [0u8; 4];
            size_bytes.copy_from_slice(&buf[1..5]);
            let data_size = u32::from_be_bytes(size_bytes);

            if data_size as usize > max_data_size {
                return Err(Error::BufferOverflow {
                    expected: data_size as usize,
                    limit: max_data_size,
                });
            }

            *buf = &buf[Self::RESPONSE_HEADER_SIZE..];
            Ok(Self { status, data_size })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_response_header() {
        let header = ResponseHeader::new(3, 1);
        let mut buf = Vec::with_capacity(ResponseHeader::RESPONSE_HEADER_SIZE);
        header.encode(&mut buf, false).unwrap();
        assert_eq!(ResponseHeader::RESPONSE_HEADER_SIZE, buf.len());
        let header2 = ResponseHeader::decode(&mut buf.as_slice(), false, 1024).unwrap();
        assert_eq!(header.status, header2.status);
        assert_eq!(header.data_size, header2.data_size);
    }

    #[test]
    fn test_response_header_decode_oversized() {
        let header = ResponseHeader::new(3, 1000);
        let mut buf = Vec::with_capacity(ResponseHeader::RESPONSE_HEADER_SIZE);
        header.encode(&mut buf, false).unwrap();
        let result = ResponseHeader::decode(&mut buf.as_slice(), false, 100);
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
