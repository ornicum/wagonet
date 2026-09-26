# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

### Added

- Unit test `server_tl_regular_command` verifying `read_command` returns `Ok(Some(...))` for regular (non-ping) commands
- Unit test `server_tls_ping_no_payload` verifying `read_command` returns `Ok(None)` for Ping

### Fixed

- Panic race condition in `ClientTL` and `ClientTLS` (`.unwrap()` after `is_none()` check replaced with proper error handling)
- Error type unification in `ClientTLS` (all methods now return `crate::Result` instead of `Box<dyn Error + Send + Sync>`)
- `#[must_use]` added to public functions where appropriate
- `#[non_exhaustive]` added to public enums (`Error`, `Command`, `ResponseStatus`)

### Removed

- Unreachable `command == 0` checks inside `while let Some(...)` loops in examples and test helpers