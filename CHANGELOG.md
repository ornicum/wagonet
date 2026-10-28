# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security Fixes

- **OOM via unbounded data_size (CRITICAL)**: `RequestHeader::decode` and `ResponseHeader::decode` now validate `data_size` against the connection's `max_buffer_size` (configurable via `set_max_buffer_size`) before any allocation, returning `Error::BufferOverflow` if exceeded. This prevents DoS via malicious oversized payloads. The separate `max_data_size` field was removed to avoid confusing duplication with `max_buffer_size`.

- **Ping old-format DoS (HIGH)**: Server now rejects Ping commands (command=0) with `data_size != 0` with `Error::Protocol("Ping must have data_size=0")`. Previously, a malicious ping response with large `data_size` could force the client to read/discard up to 4 GiB of data.

- **Non-blocking Drop (HIGH)**: `PingState::stop_ping_task()` is now synchronous (was `async fn` with 5s timeout wait). `Drop` implementations for `ClientTL` and `ClientTLS` no longer block - they only set the stop flag and abort the task handle. This eliminates potential deadlock when dropping clients in async contexts.

### Documentation

- **Minimum TLS environment (SM-07)**: document that wagonet relies on the platform TLS stack (`native-tls` / system OpenSSL); minimum supported OpenSSL is 3.0.x / 1.1.1w+; cipher and TLS-version policy follow platform defaults; rustls migration out of scope for 0.x.
- **PKCS#12 password zeroization (SM-08)**: document `set_secure` password handling is the caller's responsibility; the library does not zeroize caller-owned memory; recommend application-layer secret handling (e.g. `zeroize`).
- **No protocol-level authentication/replay/sequencing (SM-09)**: document the wire protocol provides none of these guarantees of its own; they are TLS-only; applications on untrusted paths without TLS must add nonces/sequence numbers themselves.
- **Shutdown idempotency + checked send (SM-12)**: `Server::shutdown()` now logs a `tracing::debug!` message when signaled with no active receivers instead of silently dropping; document shutdown is idempotent on `shutdown()` and `shutdown_handle()`.
- **No-answer mode compatibility limitation (SI-09)**: document that `set_command_has_answer` / `handle_message_with_no_answer` are compatible only with servers that send no response frames at all; not compatible with wagonet's own `Server`/`ServerTL`/`ServerTLS` `send_data` path; documented limitation until a wire flag is designed.
- **Keep-alive indefinite WONTFIX-by-design**: document the ping keep-alive intentionally holds idle connections open indefinitely with no idle cap; applications needing an idle timeout should drop the client.
### Refactoring

- **Removed `max_data_size` from `TimeoutConfig`**: The duplicate `max_data_size` field (which duplicated `max_buffer_size` semantics) has been removed. Data size limits now use the connection's `max_buffer_size` exclusively, configurable via `set_max_buffer_size()`. This eliminates confusion where users could increase buffer size but data would still be rejected at 10 MB.

### Breaking Changes

- **`read_command` return type changed**: `Result<(u32, usize)>` → `Result<Option<(u32, usize)>>`
  
  The function now automatically handles Ping (command=0, data_size=0) internally:
  - Sends 1-byte ACK response
  - Returns `Ok(None)` to caller
  - Caller simply continues the loop

  **Migration:**

  ```rust
  // OLD CODE (does not compile):
  while let Ok((cmd, size)) = server.read_command().await {
      let data = server.receive_data(size).await?;
      server.send_data(0, Some(&data)).await?;
  }

  // NEW CODE:
  while let Some((cmd, size)) = server.read_command().await? {
      let data = server.receive_data(size).await?;
      server.send_data(0, Some(&data)).await?;
  }
  // Ping (command=0) is handled automatically:
  // - ACK is sent inside read_command
  // - None is returned
  // - Loop simply continues
  ```

- **`PingState` fields privatized**: All 6 fields (`last_activity`, `in_flight`, `task_handle`, `should_stop`, `config`, `timeout_config`) are now private. Added getters (`last_activity()`, `in_flight()`, `config()`, `timeout_config()`, `should_stop()`, `task_handle()`) and mutation methods (`set_should_stop()`, `take_task_handle()`).

- **`TimeoutConfig::validate()` return type changed**: `()` → `Result<()>`. Now returns `Error::Config` when invariant `ping_interval < read_header` is violated.

- **`TimeoutConfig::with_ping_interval()` added**: New builder method with automatic clamping to maintain `ping_interval < read_header`.

- **`set_timeout_config()` return type changed**: `()` → `Result<()>`. May return `Error::Config` if validation fails.

  **Migration for PingState/TimeoutConfig:**

  ```rust
  // OLD CODE (does not compile):
  ping_state.config.lock().await.enabled = true;
  timeout_config.validate();  // ignoring result
  client.set_timeout_config(config).await;

  // NEW CODE:
  let mut config = ping_state.config().lock().await;
  config.enabled = true;  // via getter
  timeout_config.validate()?;  // must check result
  client.set_timeout_config(config).await?;
  ```
- **`set_max_buffer_size()` return type changed**: `()` → `Result<()>`. Now returns `Error::Config` when the buffer reservation cannot be satisfied (e.g. `usize::MAX`), instead of panicking. The object keeps its previous limit on failure.

  **Migration:**

  ```rust
  // OLD CODE (does not compile):
  client.set_max_buffer_size(usize::MAX);

  // NEW CODE:
  client.set_max_buffer_size(usize::MAX)?; // or .expect(...)
  ```

### Added

- Unit test `server_tl_regular_command` verifying `read_command` returns `Ok(Some(...))` for regular (non-ping) commands
- Unit test `server_tls_ping_no_payload` verifying `read_command` returns `Ok(None)` for Ping
- Unit tests for `TimeoutConfig::with_ping_interval()` clamping behavior
- Unit tests for `TimeoutConfig::validate()` error cases
- Unit test `pingstate_set_should_stop_and_take_task_handle` for new `PingState` methods

### Fixed

- Panic race condition in `ClientTL` and `ClientTLS` (`.unwrap()` after `is_none()` check replaced with proper error handling)
- Error type unification in `ClientTLS` (all methods now return `crate::Result` instead of `Box<dyn Error + Send + Sync>`)
- `#[must_use]` added to public functions where appropriate
- `#[non_exhaustive]` added to public enums (`Command`, `ResponseStatus`); `Error` already had it

### Removed

- Unreachable `command == 0` checks inside `while let Some(...)` loops in examples and test helpers
- Test `pingstate_fields_are_private` (was ineffective — privacy enforced by compiler)