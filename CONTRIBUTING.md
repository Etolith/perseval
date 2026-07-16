# Contributing to Perseval

Thank you for helping make agent failures easier to investigate.

## Development setup

Perseval currently targets macOS 13 or newer and uses the Rust toolchain pinned
in `rust-toolchain.toml`. Install Xcode Command Line Tools before building.

Until `traces-to-evals` is published and pinned as a normal dependency, keep
both repositories beside each other:

```text
work/
├── perseval/
└── traces-to-evals/
```

Then run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/package-macos-app.sh
```

## Change expectations

- Keep raw trace content local and bounded by default.
- Preserve immutable project, run, revision, finding, and evidence identities.
- Add regression coverage for protocol, storage, or detector behavior changes.
- Keep render and input handlers free of blocking storage work.
- Update public documentation when a cold-start user would otherwise need
  repository knowledge to complete the workflow.

Generated workspaces, downloaded datasets, benchmark output, credentials, and
internal QA notes must not be committed.

## Pull requests

Explain the user-visible problem, the chosen boundary, and the verification
performed. Small, reviewable changes are preferred. Security reports must use
the private process in `SECURITY.md`, not a public issue.
