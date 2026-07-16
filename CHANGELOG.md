# Changelog

Perseval follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
uses semantic version tags.

## Unreleased

## 0.1.2 - 2026-07-16

### Added

- Added an explicit project switcher with a read-only all-projects view, new
  project creation, and direct access to the selected project's trace sources.
- Restored the public product tour with verified screenshots for Sources, Runs,
  investigations, full-trace views, eval review, comparison, and Settings.

### Fixed

- Prevented project changes from leaving another project's Compare or Full
  Trace editor active, and made Sources follow the selected project.
- Reflowed Failure Inbox and Runs into readable cards at increased text sizes,
  including the supported 200% setting.
- Exposed every failure filter as a named radio button or checkbox for assistive
  technology.
- Replaced textual pin and project actions with bundled, consistent icons.

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
