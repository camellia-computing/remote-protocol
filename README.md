# Camellia Remote Protocol

Shared Rust protocol, transport, configuration, and file-transfer primitives used by the
Camellia Remote client and server.

The repository is versioned independently and consumed at an exact Git commit. Existing RustDesk
wire constants and protobuf fields are retained where they are required for client/server protocol
operation; they are not product branding or a compatibility promise for pre-release local data.

No Camellia-operated network endpoint is compiled into the default build. A reviewed client release
may set `CAMELLIA_REMOTE_UPDATE_URL` at build time to a version service that implements the bounded
`VersionCheckRequest`/`VersionCheckResponse` contract. When it is unset, update checks remain offline.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

`Cargo.lock` is committed because both Remote runtimes consume this repository
as a pinned source contract. CI must validate the exact reviewed registry and
Git dependency graph rather than silently resolving a newer graph. The required
CI gate also compiles Apple-specific code on a hosted macOS runner so conditional
platform implementations cannot bypass review.

See `SOURCE_PROVENANCE.json`, `NOTICE`, and `LICENSE` for origin and licensing information.
