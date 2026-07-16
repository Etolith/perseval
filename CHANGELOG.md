# Changelog

Perseval follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
uses semantic version tags.

## Unreleased

## 0.1.1 - 2026-07-16

### Fixed

- Stabilized the full-trace timeline across normal, inspector-constrained,
  compact, and increased-text layouts.
- Preserved eval review state, chronological run ordering, causal comparison,
  lifecycle visibility, semantic activation, and detector fact scoping across
  live OTLP and MCP workflows.
- Clarified error-only investigations by naming the behavioral evidence that
  instrumentation must capture before Perseval can produce an actionable
  finding.

### Changed

- Pinned `traces-to-evals` to the repaired observation-time revision.
- Made release publication depend on formatting, Clippy, tests, dependency
  auditing, secret-history scanning, application bundle verification, and
  optional signing and notarization on the tagged revision.

## 0.1.0 - 2026-07-16

### Added

- Local OTLP/HTTP JSON and protobuf ingestion with durable journaling.
- Native failure investigation, bounded full-trace tree and timeline, run
  comparison, reviewed eval definitions, and read-only MCP access.
- Optional local feature-similarity and explicitly enabled OpenAI augmentation.

### Changed

- Release preparation now pins the Rust toolchain, marks internal crates as
  non-publishable, removes superseded adapters/examples, and adds automated
  build and release gates.

### Known limitations

See `KNOWN_LIMITATIONS.md`.
