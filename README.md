# Camellia Remote Protocol

Shared Rust protocol, transport, configuration, and file-transfer primitives used by the
Camellia Remote client and server.

The repository is versioned independently and consumed at an exact Git commit. Existing RustDesk
wire constants and protobuf fields are retained where they are required for client/server protocol
operation; they are not product branding or a compatibility promise for pre-release local data.

No Camellia-operated network endpoint is compiled into the default build. The
shared contract intentionally exposes no version-service or auto-update API:
client releases are discovered and installed only through their reviewed,
immutable release process. Client-owned help links are supplied through
`CAMELLIA_REMOTE_DOCS_HOME_URL` and
`CAMELLIA_REMOTE_LINUX_DISPLAY_DOCS_URL`; the shared protocol never embeds an
organization login or a consumer repository name.

## Local secret-storage contract

Optional empty values remain empty. Every non-empty local secret must use the current
versioned, random-nonce authenticated-encryption envelope; plaintext, malformed,
wrong-key, oversized, and already-encrypted inputs are rejected instead of being
written back unchanged. Device identity and credential corruption therefore reset
the affected value rather than creating a plaintext migration path.

Provisioned permanent passwords must be supplied as the current H1 hash together
with a non-empty salt. Plaintext preset passwords are not accepted. The local
envelope is bound to the installation key pair and is intended to protect
configuration at rest within the operating-system account boundary; deployment
must also protect that account and its configuration directory.

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
