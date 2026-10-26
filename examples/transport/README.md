# Transport Examples

Low-level API: you accept connections and drive the protocol loop yourself
via ServerTL / ServerTLS and ClientTL / ClientTLS.

Use these examples when you need full control over the connection lifecycle.
For most applications the framework/ API (Server + ProtocolCommandHandler)
is simpler: see ../framework/.

- simple_server.rs / simple_client.rs - plain TCP echo
- tls_server.rs / tls_client.rs - TLS with certificate validation
- no_answer_mode.rs - fire-and-forget commands
- ping_example.rs - application-level keep-alive pings over TCP
- ping_tls_example.rs - application-level keep-alive pings over TLS
