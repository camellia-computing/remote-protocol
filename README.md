# Camellia Remote Protocol

Shared Rust protocol, transport, configuration, and file-transfer primitives used by the
Camellia Remote client and server.

The repository is versioned independently and consumed at an exact Git commit. Existing RustDesk
wire constants and protobuf fields are retained where they are required for client/server protocol
operation; they are not product branding or a compatibility promise for pre-release local data.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

See `SOURCE_PROVENANCE.json`, `NOTICE`, and `LICENSE` for origin and licensing information.
