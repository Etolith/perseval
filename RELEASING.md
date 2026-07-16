# Releasing Perseval

GitHub Actions builds and tests every pull request on macOS. Tags matching
`v<workspace-version>` start the distribution workflow.

## Before tagging

1. Run `scripts/check-release-readiness.sh`.
2. Confirm CI, dependency auditing, secret scanning, and the macOS package job
   pass on the same commit.
3. Update `CHANGELOG.md` and remove the target version from `Unreleased`.
4. Confirm the tag exactly matches the workspace version, for example
   `v0.1.1`.

## GitHub configuration

The `release` environment accepts these optional secrets for Developer ID
signing and notarization:

- `MACOS_CERTIFICATE_P12_BASE64`
- `MACOS_CERTIFICATE_PASSWORD`
- `MACOS_SIGNING_IDENTITY`
- `APPLE_ID`
- `APPLE_APP_PASSWORD`
- `APPLE_TEAM_ID`

When configured, the release job imports the certificate into an ephemeral
keychain, enables the hardened runtime, notarizes, and staples the application.
Without those secrets, the same tagged workflow publishes a clearly labeled
ad-hoc-signed beta. Both paths create a ZIP, write SHA-256 checksums, and attach
the artifacts to the GitHub release.

Do not describe an ad-hoc-signed beta as notarized. Production distribution
should configure Developer ID ownership before broad promotion.
