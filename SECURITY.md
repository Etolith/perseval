# Security policy

## Supported versions

Perseval is pre-1.0. Security fixes are applied to the latest development
release only.

## Reporting a vulnerability

Do not open a public issue for suspected vulnerabilities, exposed secrets, or
privacy failures. Use GitHub's private vulnerability reporting feature for the
`Etolith/perseval` repository. Include reproduction steps, affected versions,
and whether raw trace data or credentials may have been exposed.

If private vulnerability reporting is unavailable, do not attach sensitive
material to a public discussion. Ask a maintainer through the repository's
public contact channel to establish a private reporting route.

## Security boundaries

Perseval binds OTLP to loopback by default, stores workspaces locally, keeps MCP
read-only, and does not reveal raw payload bodies through MCP. Optional hosted
analysis is disabled until explicitly configured.

## Temporary advisory exceptions

The security workflow temporarily ignores `RUSTSEC-2026-0194` and
`RUSTSEC-2026-0195` for `quick-xml` 0.30 and 0.39. Those versions enter the
locked graph only through `xcb` and `wayland-scanner`, Linux display-backend
build dependencies. Perseval currently publishes a macOS application and does
not compile or execute those XML parsers in the supported artifact.

These exceptions are not a claim that the vulnerable versions are safe. They
must be removed before Perseval ships a Linux artifact, or earlier when the
GPUI dependency graph upgrades to `quick-xml` 0.41 or later. Applicable
vulnerabilities remain fatal; unmaintained or yanked transitive-package
warnings are reported without being treated as vulnerabilities.
