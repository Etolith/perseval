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
