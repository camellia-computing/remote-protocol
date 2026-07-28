# Repository instructions

- Preserve protobuf field numbers and functional wire constants unless client and server are changed
  together and contract tests prove the new protocol.
- Do not add local-data or legacy-brand compatibility layers before the first production release.
- Do not introduce `unwrap()` or `expect()` in production code; propagate or handle errors.
- Never commit credentials, signing material, generated build output, or machine-local state.
- Required gates are formatting, Clippy with warnings denied, tests, dependency audit, and provenance
  validation.
