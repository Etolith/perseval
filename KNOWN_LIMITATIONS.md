# Known limitations

Perseval is an early local-first release. These limitations are part of the
current product contract rather than hidden release notes.

## Startup and ingestion

- A process already using the configured OTLP address stops startup. Set
  `PERSEVAL_OTLP_BIND` to another loopback address before relaunching.
- OTLP/HTTP JSON and protobuf are supported. OTLP/gRPC is not yet supported.
- A trace is analyzed only after its idle and finalization windows complete.

## Findings, comparison, and evals

- Explicit `oldest` ordering is not reliably chronological in every case.
- Structural comparison can report a changed root outcome before the later
  causal tool-step divergence.
- Accepted eval candidates are immutable reviewed definitions. Runnable
  fixtures, execution, and remediation-verification reports are not shipped.
- Comparison is evidence for a reviewer, not a rollout or remediation decision.

## Accessibility

- Keyboard paths and accessible labels cover the principal workflow, but some
  row affordances still lack independent VoiceOver activation.
- Human VoiceOver speech-order validation remains a release gate.

## Optional AI providers

- OpenAI features require explicit settings and a process-level
  `OPENAI_API_KEY`; repository `.env` files are not loaded automatically.
- Provider retry state is process-local rather than a durable job ledger.
- Structured-only projections are used. There is no content-summary preview
  and consent flow.
- A held-out quality report has not established an improvement over the
  deterministic baseline.

## Learned quality checks

- The first learned evaluator family is Task Completion. Hallucination,
  safety/policy adherence, tool-use correctness, usefulness, frustration, and
  step-efficiency checks are not shipped.
- Learned assessments operate only on finalized immutable trace revisions.
  Live or provisional learned evaluation is not shipped.
- Review queues and calibration are local product workflows. The current MCP
  catalog cannot run a quality check, submit a human answer, fit a calibration
  release, or activate a threshold policy.
- A quality check may finish with failed, unavailable, abstained,
  privacy-blocked, budget-blocked, or not-applicable states. Those states are
  not silently converted to pass or fail.
- Automatic finding promotion remains blocked until the calibration screen's
  human-label, agreement, precision, and negative-predictive-value gates pass.
- The frozen Arize head-to-head is an engineering baseline, not evidence for a
  public superiority, scale, or general-domain accuracy claim.

## MCP

- The MCP catalog is read-only. Compute, mutation, and raw-payload reveal
  permissions are not exposed.
