# Roadmap to Open Source Publication

## Current Status: 3/10 ready

---

Working protocol with dynamic headers (1 byte for data_size=0, 5 bytes for data_size>0)
Time limits on all operations (connect, read_header, read_data, write)
TLS + plain TCP support
Basic tests passing (6 tests)
Async/await on tokio
Symmetric client/server logic
command_has_answer flag for no-answer mode
TCP keep-alive support (socket2) with configurable idle/interval
Connection reuse with bounded retry (1 retry) and health-aware reconnection
Secure TLS defaults (accept_invalid_certs=false)
---

## CRITICAL (Must Have Before Publishing)

### Documentation

- README.md with project description, features, installation, quick start, API overview, configuration
- examples/ directory with simple_client.rs, simple_server.rs, tls_client.rs, tls_server.rs, no_answer_mode.rs
- API documentation with /// doc comments on all public structs/methods
- **Architecture documentation: docs/architecture.md** (component diagrams, state machines, sequence diagrams, security analysis)
- LICENSE file (MIT or Apache-2.0)
### Cargo.toml Metadata

Add these fields to Cargo.toml:

- description = "High-performance async TCP/TLS transport library with custom protocol"
- repository = "https://github.com/yourusername/wagonet"
- documentation = "https://docs.rs/wagonet"
- homepage = "https://github.com/yourusername/wagonet"
- license = "MIT OR Apache-2.0"
- keywords = ["tcp", "tls", "async", "transport", "protocol"]
- categories = ["network-programming", "asynchronous"]
- readme = "README.md"
- include = ["src/**/*", "examples/**/*", "LICENSE-*", "README.md"]

### CI/CD Pipeline

Create .github/workflows/ci.yml with:

- Build on stable, beta, nightly Rust
- Run tests
- Run clippy
- Run rustfmt check
- Test coverage (optional)

### Project Structure

- .gitignore (target/, Cargo.lock for libs, etc.)
- .editorconfig (consistent formatting)
- CHANGELOG.md (version history)
- CONTRIBUTING.md (how to contribute)

---

## IMPORTANT (Quality & Best Practices)

### Error Handling

- Replace Box<dyn Error> with custom error types
- Use thiserror crate for derive macros
- Create TransportError enum with variants: Connection, Timeout, Protocol, Io, Tls

### API Improvements

- Replace &Vec<u8> with &[u8] in all signatures
- Replace Vec<u8> returns with impl AsRef<[u8]> where possible
- Add builder pattern for configuration
- Make max_buffer_size configurable per-message

### Logging

- Replace all println! with tracing::warn! or tracing::info!
- Add structured logging fields
- Document log levels used

### Testing

Edge case tests:

- Connection drop mid-message
- Very large messages (100MB+)
- Invalid/malformed bytes
- Concurrent connections
- Timeout scenarios

Integration tests:

- Client-server roundtrip with various data sizes
- TLS certificate validation
- Reconnection scenarios

Property-based tests (optional, using proptest):

- Random message sizes
- Random command IDs
- Fuzz-like testing

---

## NICE TO HAVE (Production-Ready Features)

### Connection Management

- Automatic reconnection with exponential backoff
- Connection pooling (optional)
- Health checks / heartbeat mechanism
- Graceful shutdown support

### Performance

- Benchmarks using criterion (message throughput, latency percentiles, memory usage)
- Zero-copy optimizations where possible
- Buffer pooling for high-throughput scenarios

### Security

- Fuzzing tests using cargo-fuzz (protocol parsing, header decoding)
- Rate limiting (optional)
- Message size validation at protocol level

### Extensibility

Feature flags in Cargo.toml:

- default = ["tls"]
- tls = ["native-tls", "tokio-native-tls"]
- compression = ["flate2"]

Additional features:

- Compression support (optional, via feature flag)
- Custom serializers trait (beyond just bytes)

### Observability

- Metrics integration (optional): messages sent/received, connection duration, error rates
- Tracing spans for request lifecycle

---

## SUGGESTED TIMELINE

### Week 1: "Showable to People"

Tasks:

- README + examples + LICENSE
- Cargo.toml metadata
- CI pipeline
- Basic thiserror migration
- &[u8] fixes

Result: Library looks professional on crates.io

### Week 2: "Usable in Production"

Tasks:

- Edge case tests
- Reconnection logic
- Heartbeat mechanism
- API documentation
- Logging cleanup

Result: Library is reliable for real use

### Week 3: "Something to Be Proud Of"

Tasks:

- Benchmarks
- Fuzzing
- Feature flags
- Compression (optional)
- Performance optimizations

Result: Library competes with established alternatives

---

## PRIORITY ORDER

1. README + examples (immediate impact)
2. CI/CD (prevents regressions)
3. Error types (better UX)
4. Edge case tests (reliability)
5. Reconnection (production necessity)
6. Benchmarks (performance visibility)
7. Everything else (nice-to-have polish)

---

## NOTES

- Current timeout values (600s) are too high for production - consider 60-120s
- Consider semantic versioning strategy (0.x for breaking changes, 1.0 for stable API)
- Plan for backward compatibility once 1.0 is released

---

## RESOURCES

- Rust API Guidelines: https://rust-lang.github.io/api-guidelines/
- crates.io publishing guide: https://doc.rust-lang.org/cargo/reference/publishing.html
- Semantic Versioning: https://semver.org/
- Good README examples: https://github.com/rust-lang/rust/blob/master/README.md
