# Framework Examples

High-level API: implement ProtocolCommandHandler with your business logic,
register it via Server::set_protocol_command_handler_factory, and the server
handles connections, protocol framing, keep-alive pings and shutdown for you.

- high_level_server.rs / high_level_client.rs - multi-command echo server
  with per-connection state, error handling and connection finish
- shutdown_example.rs - graceful shutdown via shutdown_handle()
