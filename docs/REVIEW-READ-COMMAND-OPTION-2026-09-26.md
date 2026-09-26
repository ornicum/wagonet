# Code Review: read_command returns Option — 2026-09-26

## Baseline
- build: 0 warnings
- clippy: 0 warnings  
- test: 22 passed, 1 ignored (backward_compat_new_client_old_server — intentional)
- git diff: 9 files changed, 87 insertions(+), 109 deletions(-)

## Verdict: REQUIRES FIXES

### P1 (critical before release)
1. examples/ping_example.rs and examples/ping_tls_example.rs: dead code
   `if command == 0 && data_size == 0` inside `while let Some(...)` — Ping
   returns None, this branch is unreachable. Misleads users.

### P2 (cleanup)
2. src/client_tl.rs:559-580 (run_mock_server_tl): duplicate logic —
   command == 0 handling in Some branch never executes.
3. tests/integration_tests.rs:187-205: same issue — ping counter
   incremented in both Some (impossible) and None branches.

### P3 (documentation)
4. README.md: outdated read_command signature in API Reference section.
5. Missing CHANGELOG.md / UPGRADING.md with migration guide.

### Recommendation
6. Add unit-test server_tl_regular_command in src/server_tl.rs (TLS has
   server_tls_ping_no_payload but TL lacks equivalent).

## New API contract
- `read_command` returns `Ok(None)` for Ping (command 0, data_size 0),
  sending 1-byte ACK internally.
- Returns `Ok(Some((command, data_size)))` for regular commands.
- Pattern: `while let Some((cmd, size)) = server.read_command().await?`

## Migration guide for external users
```rust
// OLD CODE (won't compile):
while let Ok((cmd, size)) = server.read_command().await {
    let data = server.receive_data(size).await?;
    server.send_data(0, Some(&data)).await?;
}

// NEW CODE:
while let Some((cmd, size)) = server.read_command().await? {
    let data = server.receive_data(size).await?;
    server.send_data(0, Some(&data)).await?;
}
// Ping handled automatically: ACK sent, None returned, loop continues.
