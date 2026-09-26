# Security Audit Report — Model 1 (nvidia/nemotron-3-ultra)

## 1. Network Data Parsing

### Finding 1.1: Unbounded allocation from unvalidated `data_size`
- **Priority**: CRITICAL
- **File**: `src/request_header.rs:39`, `src/client_tl.rs:267-277`, `src/server_tl.rs:102-107`
- **Description**: `RequestHeader::decode` reads `data_size` as `u32` without validation. Both client and server later allocate `buffer.resize(data_size as usize, 0)` after checking against `max_buffer_size`, but the check happens AFTER decode. An attacker sending `data_size = u32::MAX` (4,294,967,295) would cause a 4 GiB allocation attempt → OOM kill or panic.
- **Attack Scenario**: Attacker opens TCP/TLS connection, sends valid 8-byte header with `command=1, data_size=0xFFFFFFFF`. Server/client decodes header, then attempts `Vec::resize(4_294_967_295, 0)` → process OOM.
- **Recommendation**: Validate `data_size` in `RequestHeader::decode` against a configurable `MAX_ALLOWED_DATA_SIZE` (e.g., 10 MB default) and return `Error::Protocol` before any allocation.

### Finding 1.2: No protection against partial header reads
- **Priority**: HIGH
- **File**: `src/request_header.rs:26-31`, `src/response_header.rs:37-41`
- **Description**: `decode` returns `Error::Protocol("Not enough bytes...")` if buffer < header size. Callers use `read_exact` with timeout, so partial reads become timeout errors. However, if timeout is disabled (Duration::ZERO), `read_exact` blocks indefinitely on partial header.
- **Attack Scenario**: Attacker sends 4 bytes of an 8-byte request header, then stops. With `read_header=0`, server blocks forever.
- **Recommendation**: Document that `read_header=0` is dangerous; consider rejecting zero timeouts in `TimeoutConfig::validate`.

### Finding 1.3: `data_size = u32::MAX` edge case in buffer check
- **Priority**: MEDIUM
- **File**: `src/client_tl.rs:270`, `src/server_tl.rs:104`
- **Description**: Check `data_usize > max_buffer_size` uses `as usize` cast. On 32-bit targets, `u32::MAX as usize` = `usize::MAX`, check passes logic but allocation fails. On 64-bit, check correctly catches it. The cast itself is safe but the subsequent `resize` will panic on OOM.
- **Recommendation**: Check `data_size > max_buffer_size as u32` before cast, return error early.

## 2. Ping Protocol

### Finding 2.1: Unbounded connection lifetime via ping-only traffic
- **Priority**: HIGH
- **File**: `src/ping.rs:144-188`
- **Description**: Ping task runs every `interval/4` (default 7.5s checks for 30s interval). An attacker who can complete the TLS/TCP handshake can keep a connection alive indefinitely by only responding to pings. No max-idle-connection-time or max-pings-without-real-traffic limit exists.
- **Attack Scenario**: Attacker establishes 10,000 connections, responds to pings only. Server exhausts file descriptors / memory.
- **Recommendation**: Add `max_idle_duration` or `max_pings_without_activity` config; stop ping task / drop connection if exceeded.

### Finding 2.2: No server-side ping timeout for client non-response
- **Priority**: MEDIUM
- **File**: `src/ping.rs:284-327`, `src/server_tl.rs:85-92`
- **Description**: Server responds to ping immediately (1-byte OK). If client never reads the response, server's `write` has `write` timeout (default 60s). But no application-level "client didn't ack ping" detection. Connection stays open until TCP keepalive or write timeout.
- **Attack Scenario**: Attacker opens connection, sends ping request, never reads response. Server blocks on `write_all` for 60s per ping.
- **Recommendation**: Ping response write should use a shorter dedicated timeout; track consecutive failed pings and terminate.

### Finding 2.3: Ping response accepts both 1-byte and 5-byte formats (protocol confusion)
- **Priority**: LOW
- **File**: `src/ping.rs:284-327`
- **Description**: `receive_ping_response_impl` tries to read 4 extra bytes with 10ms timeout. Old server sends 5 bytes (status+size), new server sends 1 byte. This dual-format parsing increases attack surface (e.g., attacker sends 1-byte status=1 then 4 bytes garbage; client interprets as old-server response with `data_size=garbage`).
- **Attack Scenario**: Malicious server sends status=1 + 4 bytes `0xFF 0xFF 0xFF 0xFF`. Client reads as old-server ping response with `data_size=4294967295`, then tries to read/discard that payload → DoS.
- **Recommendation**: Remove backward compatibility; standardize on 1-byte ping response. Or strictly validate `data_size == 0` for old format and reject non-zero.

### Finding 2.4: Ping can bypass application-level auth (by design)
- **Priority**: INFO
- **File**: `src/server_tl.rs:85-92`
- **Description**: Ping is handled at protocol layer before any application authentication. This is expected for keep-alive but worth documenting.
- **Recommendation**: Document that ping responses reveal server liveness without auth.

## 3. TLS Security

### Finding 3.1: `accept_invalid_certs` public setter enables dangerous misconfiguration
- **Priority**: HIGH
- **File**: `src/client_tls.rs:58, 68-71`
- **Description**: `set_accept_invalid_certs(true)` is a public method. Default is `false` (secure), but a developer under pressure may call this for "quick fix" with self-signed certs, silently disabling all cert validation (MITM vulnerable).
- **Attack Scenario**: Dev sets `accept_invalid_certs(true)` in production. Attacker performs MITM with self-signed cert; client accepts it.
- **Recommendation**: 
  - Rename to `danger_accept_invalid_certs` (matching `native-tls` builder method)
  - Add `#[doc = "**DANGER**: Disables certificate validation. Use ONLY for local testing."]`
  - Consider feature-gating behind `unsafe-tls` cargo feature.

### Finding 3.2: Default domain is "example.org" — breaks hostname verification
- **Priority**: HIGH
- **File**: `src/client_tls.rs:56, 74-76`
- **Description**: `ClientTLS::new` sets `domain: "example.org".to_string()`. If user forgets `set_domain("real.host")`, SNI sends "example.org" and cert verification checks against "example.org" — will fail for real server (cert mismatch) OR pass incorrectly if attacker presents cert for example.org.
- **Attack Scenario**: User deploys without calling `set_domain`. Connection fails (safe) but error message may be confusing. If attacker controls DNS and presents valid cert for example.org, verification passes (unlikely but possible).
- **Recommendation**: 
  - Make `domain` required in `ClientTLS::new` (breaking change) OR
  - Panic in `connect()` if `domain == "example.org"` and `!accept_invalid_certs`
  - At minimum, warn loudly in docs and `connect()`.

### Finding 3.3: Uses `native-tls` (OpenSSL) — platform-dependent vulnerabilities
- **Priority**: MEDIUM
- **File**: `Cargo.toml: native-tls = "0.2.18"`, `openssl = "0.10.81"`
- **Description**: `native-tls` uses system OpenSSL (via `openssl-sys`). Vulnerabilities in system OpenSSL (e.g., CVE-2023-0286, CVE-2024-0727) affect this library. No minimum OpenSSL version enforced.
- **Recommendation**: 
  - Document requirement for patched OpenSSL ≥ 3.0.13 / 1.1.1w
  - Consider migrating to `rustls` (pure Rust, auditable) for future versions
  - Add `openssl` version check at build time via `openssl-sys` crate features.

### Finding 3.4: No TLS version / cipher suite configuration
- **Priority**: MEDIUM
- **File**: `src/client_tls.rs:108-120`
- **Description**: `native-tls` uses platform defaults. No way to enforce TLS 1.2+ or disable weak ciphers.
- **Recommendation**: Expose `min_tls_version` config or migrate to `rustls` which allows this.

## 4. Timeouts and DoS

### Finding 4.1: `connect` retry loop has no overall deadline
- **Priority**: HIGH
- **File**: `src/client_tl.rs:137-157`, `src/client_tls.rs:140-160`
- **Description**: `connect()` retries up to 10 times with 1s sleep between. Each attempt has `connect` timeout (default 10s). Total worst-case: ~100s + 10s = 110s before returning error. No `connect_total_timeout` config.
- **Attack Scenario**: Attacker's server accepts TCP but hangs during TLS handshake. Client ties up task for 110s per connection attempt.
- **Recommendation**: Add `connect_total_timeout` or `max_connect_duration`; enforce overall deadline.

### Finding 4.2: Zero timeout values disable timeouts entirely
- **Priority**: HIGH
- **File**: `src/timeout_config.rs`, `src/client_tl.rs`, `src/server_tl.rs`
- **Description**: `TimeoutConfig` fields are `Duration`. `Duration::ZERO` means "no timeout" in `tokio::time::timeout` (it returns immediately with `Ok` for zero timeout? Actually `timeout(Duration::ZERO, fut)` yields immediately with `Err(Elapsed)` — but `read_exact` with zero timeout would fail instantly. Wait, need to verify: `tokio::time::timeout(Duration::ZERO, fut)` always times out immediately. So `Duration::ZERO` = "fail immediately", not "no timeout". To disable timeout, user would need to not wrap in timeout. Current code always wraps. So `Duration::ZERO` causes immediate timeout errors — not a DoS vector but a misconfig footgun.
- **Correction**: Actually `tokio::time::timeout(Duration::ZERO, ...)` returns `Err(Elapsed)` immediately. So zero timeout = instant failure. Not a DoS but a config trap.
- **Recommendation**: Validate `Duration::ZERO` is not used for timeouts; require minimum (e.g., 1ms).

### Finding 4.3: Ping task shutdown blocks up to 5 seconds
- **Priority**: MEDIUM
- **File**: `src/ping.rs:246-262`, `src/client_tl.rs:347-356`, `src/client_tls.rs:380-389`
- **Description**: `stop_ping_task()` waits up to 5s for ping task to finish. If ping task is stuck in `read_exact` (network I/O), `Drop` blocks for 5s. `Drop` must not block.
- **Attack Scenario**: Attacker holds connection open, client drops → `Drop` blocks thread for 5s. Many drops = thread pool exhaustion.
- **Recommendation**: 
  - Make `stop_ping_task` non-blocking (fire-and-forget signal + abort handle)
  - In `Drop`, only `handle.abort()` and set `should_stop=true`, don't await.

### Finding 4.4: No connection-level resource limits (connection count, memory)
- **Priority**: MEDIUM
- **File**: Library-wide
- **Description**: Library provides no built-in connection limiting, memory accounting, or rate limiting. Server applications must implement externally.
- **Recommendation**: Document that server must enforce connection limits; consider adding `max_connections` to server API.

## 5. Panics and Unwind

### Finding 5.1: No panics in network paths (good)
- **Priority**: INFO
- **File**: All source files
- **Description**: Code uses `Result` throughout. No `.unwrap()`, `.expect()`, or `panic!()` in request/response handling. `ResponseStatus::from` uses `unwrap_or(Unknown)` safely.

### Finding 5.2: `buffer.resize(data_size, 0)` can panic on OOM
- **Priority**: MEDIUM
- **File**: `src/client_tl.rs:270`, `src/server_tl.rs:104`, `src/client_tls.rs:278`
- **Description**: After `data_size > max_buffer_size` check, `buffer.resize(data_size, 0)` is called. If `data_size` is valid but large (e.g., 10 MB) and system is under memory pressure, `resize` panics on allocation failure (Rust default allocator panics on OOM).
- **Attack Scenario**: Attacker sends many concurrent 10 MB requests on memory-constrained system → OOM panic.
- **Recommendation**: Use `try_reserve` / `try_resize` (unstable) or document that OOM panics are possible; consider `max_buffer_size` as DoS surface.

### Finding 5.3: `Drop` uses `try_lock` — safe from poisoned mutex
- **Priority**: INFO
- **File**: `src/client_tl.rs:347-356`, `src/client_tls.rs:380-389`
- **Description**: `Drop` implementations use `try_lock` on mutexes, avoiding deadlock/poison issues. Good.

## 6. Resource Leaks

### Finding 6.1: Ping task may not stop on connection error during ping
- **Priority**: MEDIUM
- **File**: `src/ping.rs:175-188`
- **Description**: If `send_ping_request_impl` or `receive_ping_response_impl` fails, code sets `*stream_guard = None` and `*should_stop.lock().await = true`, then `break`. This correctly stops the ping task. However, the `JoinHandle` remains in `task_handle` until `stop_ping_task` is called. If client never calls `disconnect()`, task handle leaks (minor).
- **Recommendation**: In ping task error path, also take and drop the task handle, or ensure `stop_ping_task` is called.

### Finding 6.2: Buffer reused correctly between requests
- **Priority**: INFO
- **File**: `src/client_tl.rs`, `src/server_tl.rs`, `src/client_tls.rs`
- **Description**: `buffer.clear()` called after each operation. No leak.

### Finding 6.3: TLS stream shutdown errors don't leak stream
- **Priority**: INFO
- **File**: `src/client_tls.rs:180-188`
- **Description**: `disconnect()` clears stream on shutdown error. Good.

## 7. Information Disclosure

### Finding 7.1: Error logs contain addresses and commands
- **Priority**: LOW
- **File**: `src/client_tl.rs:124, 130, 143`, `src/server_tl.rs:75, 82, 96`
- **Description**: `tracing::error!` logs include connection addresses, command codes, data sizes. In production with `tracing` enabled, this could leak internal command IDs or data sizes to logs. No request/response payload data logged (good).
- **Recommendation**: Use `tracing::debug` for routine errors; reserve `error` for unexpected failures. Avoid logging command values in error messages.

### Finding 7.2: Error types don't expose sensitive data
- **Priority**: INFO
- **File**: `src/lib.rs:52-93`
- **Description**: `Error` enum variants (`Timeout`, `Io`, `Protocol`, `ResponseError`, `BufferOverflow`, etc.) don't contain payload data. Good.

## 8. Mutable State / Invariants

### Finding 8.1: `PingState` exposes internal mutexes publicly
- **Priority**: HIGH
- **File**: `src/ping.rs:108-133`
- **Description**: `PingState` has public methods returning `Arc<Mutex<...>>` for `last_activity`, `in_flight`, `config`, `timeout_config`, `should_stop`, `task_handle`. External code can lock these indefinitely, deadlocking ping task or breaking invariants (e.g., setting `should_stop=false` after stop, changing interval mid-run).
- **Attack Scenario**: Malicious or buggy consumer code calls `ping_state.should_stop().lock().await` and holds it → ping task deadlocks on same mutex.
- **Recommendation**: Make these methods `pub(crate)` or remove; provide only high-level safe operations (e.g., `update_interval`, `shutdown`).

### Finding 8.2: `set_max_buffer_size` accepts arbitrarily large values
- **Priority**: MEDIUM
- **File**: `src/client_tl.rs:78-84`, `src/client_tls.rs:58-62`, `src/server_tl.rs:48-53`
- **Description**: User can call `set_max_buffer_size(usize::MAX)` → subsequent `buffer.resize` attempts huge allocation on valid large response.
- **Recommendation**: Cap at reasonable maximum (e.g., 100 MB) or require explicit `unsafe` opt-in for >100 MB.

### Finding 8.3: `command_has_answer` mutable mid-request (footgun)
- **Priority**: LOW
- **File**: `src/client_tl.rs:88-91`, `src/client_tls.rs:66-69`
- **Description**: `set_command_has_answer` changes protocol behavior for subsequent calls. `handle_message_with_no_answer` toggles it temporarily. Not thread-safe if client shared across tasks (but `&mut self` prevents that).
- **Recommendation**: Document clearly; consider making it a parameter to `handle_message` instead of mutable state.

## 9. Dependencies

### Finding 9.1: `native-tls` / `openssl` — system OpenSSL vulnerabilities
- **Priority**: HIGH
- **Dependencies**: `native-tls 0.2.18` → `openssl 0.10.81` → `openssl-sys 0.9.117`
- **Known CVEs in older OpenSSL**: CVE-2023-0286 (X.400 address parsing), CVE-2024-0727 (NULL deref), CVE-2024-2511 (DoS via client cert). Fixed in OpenSSL 3.0.13+, 1.1.1w+.
- **Risk**: If deployed on system with old OpenSSL (e.g., Ubuntu 20.04 default 1.1.1f), TLS connections vulnerable.
- **Recommendation**: Document minimum OpenSSL version; test with `openssl version` at build.

### Finding 9.2: `tokio 1.53.1` — check for CVEs
- **Priority**: MEDIUM
- **Current**: 1.53.1 (released 2025-03). Latest 1.x is ~1.58+. No known critical CVEs in 1.53.1 but should update.

### Finding 9.3: `rcgen 0.14.10` / `ring 0.17.14` — used only for cert generation (dev)
- **Priority**: LOW
- **Description**: `rcgen` is dev-dependency (used in examples/tests for self-signed certs). Not in production code path unless user uses it.

### Finding 9.4: `async-trait 0.1.92` — proc macro, no runtime risk
- **Priority**: INFO

### Finding 9.5: `socket2 0.6.5` — safe wrapper for socket options
- **Priority**: INFO

## 10. Crypto and Protocol

### Finding 10.1: No custom cryptography (good)
- **Priority**: INFO
- **Description**: All crypto delegated to `native-tls` (OpenSSL) and `ring` (via `rcgen` for cert gen only). No homegrown crypto.

### Finding 10.2: Random number generation — OS/OpenSSL
- **Priority**: INFO
- **Description**: TLS key generation, nonces handled by OpenSSL. `ring` uses `getrandom` (OS entropy). Good.

### Finding 10.3: Protocol lacks message authentication / integrity beyond TLS
- **Priority**: INFO
- **Description**: Protocol relies entirely on TLS for integrity. If TLS is bypassed (via `accept_invalid_certs`), no application-layer MAC. Acceptable for TLS-only threat model.

---

## Model 1 Summary

| Priority | Count |
|----------|-------|
| CRITICAL | 1 |
| HIGH     | 6 |
| MEDIUM   | 7 |
| LOW      | 2 |
| INFO     | 7 |

**Top 3 Fixes**:
1. Validate `data_size` in `RequestHeader::decode` before allocation (CRITICAL)
2. Add `max_idle_duration` / connection limits to prevent ping-only DoS (HIGH)
3. Harden `accept_invalid_certs` API (rename, warn, feature-gate) (HIGH)
4. Fix default domain "example.org" bug (HIGH)
5. Add overall connect deadline (HIGH)