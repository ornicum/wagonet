use strum_macros::{Display, FromRepr};

/// Protocol command codes.
///
/// This enum is non-exhaustive: additional command codes may be added in
/// future releases without a breaking change. External `match` statements
/// must include a catch-all arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u32)]
pub enum Command {
    /// Ping command (empty request/response for keep-alive).
    Ping = 0,
}

impl From<Command> for u32 {
    fn from(cmd: Command) -> Self {
        cmd as u32
    }
}

/// Response status codes.
///
/// This enum is non-exhaustive: additional status codes may be added in
/// future releases without a breaking change. External `match` statements
/// must include a catch-all arm.
#[derive(Debug, Display, Clone, Copy, FromRepr, PartialEq)]
#[non_exhaustive]
#[repr(u8)]
pub enum ResponseStatus {
    #[strum(serialize = "Unknown error")]
    Unknown = 0,
    #[strum(serialize = "No error")]
    Ok = 1,
    #[strum(serialize = "Finish")]
    Finish = 2,
    #[strum(serialize = "Error")]
    Error = 3,
    #[strum(serialize = "Database error")]
    DatabaseError = 4,
    #[strum(serialize = "Invalid command")]
    InvalidCommand = 5,
    #[strum(serialize = "Invalid data")]
    InvalidData = 6,
    #[strum(serialize = "Invalid data size")]
    InvalidDataSize = 7,
    #[strum(serialize = "Invalid data format")]
    InvalidDataFormat = 8,
    #[strum(serialize = "Invalid data value")]
    InvalidDataValue = 9,
    #[strum(serialize = "Invalid domain")]
    InvalidDomain = 10,
    #[strum(serialize = "Invalid request")]
    InvalidRequest = 11,
    #[strum(serialize = "Invalid response")]
    InvalidResponse = 12,
    #[strum(serialize = "Invalid signature")]
    InvalidSignature = 13,
}

impl From<ResponseStatus> for u8 {
    fn from(value: ResponseStatus) -> Self {
        value as u8
    }
}

impl From<ResponseStatus> for u32 {
    fn from(value: ResponseStatus) -> Self {
        value as u32
    }
}

impl From<u8> for ResponseStatus {
    fn from(value: u8) -> Self {
        Self::from_repr(value).unwrap_or(Self::Unknown)
    }
}

impl From<u32> for ResponseStatus {
    fn from(status: u32) -> Self {
        Self::from_repr(status as u8).unwrap_or(Self::Unknown)
    }
}

pub trait ToBytes {
    fn to_bytes(&self) -> Vec<u8>;
}

pub trait FromBytes {
    fn from_bytes(bytes: &[u8]) -> Self;
}
