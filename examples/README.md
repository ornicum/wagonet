# Examples

Examples are grouped by category:

- transport/ - low-level TCP/TLS transport API (ServerTL / ServerTLS, ClientTL / ClientTLS).
  You drive the protocol loop yourself.
- framework/ - high-level server framework (Server + ProtocolCommandHandler).
  The server drives the protocol loop; you implement only business logic.
- prost/ - Protocol Buffers payload over the framework API.

## transport

Terminal 1, then terminal 2:

    cargo run --example simple_server
    cargo run --example simple_client

    cargo run --example tls_server
    cargo run --example tls_client

Standalone:

    cargo run --example no_answer_mode
    cargo run --example ping_example
    cargo run --example ping_tls_example

## framework

Terminal 1, then terminal 2:

    cargo run --example high_level_server
    cargo run --example high_level_client

Standalone:

    cargo run --example shutdown_example

## prost

See prost/README.md for details.

    cargo run --example prost_server
    cargo run --example prost_client
