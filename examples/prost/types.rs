//! Pre-generated Prost types for the greet.proto example.
//! This avoids requiring `protoc` at build time.
//!
//! If you need to modify the messages, edit proto/greet.proto and regenerate
//! with: protoc --prost_out=. proto/greet.proto

/// Request to greet someone
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct GreetRequest {
    #[prost(string, tag = "1")]
    pub name: ::prost::alloc::string::String,
    /// How many times to greet
    #[prost(uint32, tag = "2")]
    pub times: u32,
}

/// Response with greetings
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct GreetResponse {
    #[prost(string, repeated, tag = "1")]
    pub greetings: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    #[prost(uint64, tag = "2")]
    pub total_greetings: u64,
}

/// Request to get server stats
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct StatsRequest {}

/// Response with server statistics
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct StatsResponse {
    #[prost(uint64, tag = "1")]
    pub total_requests: u64,
    #[prost(uint64, tag = "2")]
    pub active_connections: u64,
}
