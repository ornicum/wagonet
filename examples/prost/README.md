# Prost Example

Demonstrates wagonet with Protocol Buffers serialization (via Prost).

## Files

- greet.proto - protocol definition (source of truth)
- types.rs - pre-generated Rust types (no protoc needed at build time)
- server.rs - server implementation
- client.rs - client implementation

## Running

Terminal 1:

    cargo run --example prost_server

Terminal 2:

    cargo run --example prost_client

## Modifying the protocol

1. Edit greet.proto
2. Regenerate Rust code with protoc (requires protoc and prost-build):

       protoc --prost_out=. greet.proto

3. Or manually update types.rs to match
