# Changelog

Perseval follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
uses semantic version tags.

## Unreleased

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
