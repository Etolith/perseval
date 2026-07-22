# Learned Task Completion

Perseval's first learned quality-check workflow asks whether an agent completed
the task represented by a finalized trace. It is separate from deterministic
findings: an assessment records model evidence and uncertainty, while a finding
continues to represent a detector-supported behavioral failure.

## What the workflow preserves

Every quality check and result is versioned. A completed assessment is bound to
the project, logical trace, immutable revision, evaluator release, agent-context
binding, projection hash, provider/model configuration, execution state,
latency, token and priceable cost data, explanation, and cited evidence.

Provider failure or missing context is visible as a typed terminal state. It is
never silently counted as a pass or failure. Historical assessments remain
readable after a quality check, context release, calibration release, or policy
changes.

## Run a quality check

1. Open **Evals**, then select **Quality checks**.
2. Publish the Task Completion quality check. Its evaluator release, criteria,
   mapping, provider model, projection policy, output schema, and abstention
   behavior become immutable.
3. Preview a backfill before starting it. Review the exact target count,
   exclusions, estimated work, and outbound-content policy.
4. Start the job from that preview. The job keeps attempted, completed, failed,
   unavailable, abstained, privacy-blocked, budget-blocked, and not-applicable
   accounting separate.
5. Open a completed assessment from the trace or continue to **Review Queue**.

Only finalized immutable trace revisions are eligible. A new trace revision or
quality-check release produces a new assessment identity instead of mutating an
old result.

## Create human ground truth

Use **Review Queue** in one of two modes:

- **Blind calibration** hides automated output and peer answers until the
  reviewer submits a label, rationale, and evidence. Cases are split by stable
  leakage group so related revisions cannot cross the fit, calibration, and held-out
  test partitions.
- **Visible triage** reveals the automated result for investigation. It is
  deliberately excluded from agreement, calibration fitting, and held-out
  quality metrics.

The saved annotation is immutable and remains attached to its annotation-schema
release and exact trace revision. **Open exact trace** navigates to that frozen
revision. In the trace inspector, select **Reviews**, then open the automated
evidence citation; it resolves to the submitted projection's span and byte
range.

## Calibrate decisions safely

Open **Calibration** after blind reviews exist. Perseval keeps fit, calibration,
and held-out test roles explicit and reports confusion with the positive class,
sample denominators, agreement, Brier score, reliability, abstention,
risk/coverage, and available slices.

Activating a threshold policy materializes a new immutable decision for each
eligible assessment. It does not edit the provider's score, the assessment, or
the human answer. Automation remains blocked until every displayed gate passes,
including minimum label counts, class support, reviewer agreement,
flagged-failure precision, and auto-pass negative predictive value.

## Privacy and trust boundaries

- Trace content is untrusted evidence, never an instruction.
- The evaluator receives only the quality check's versioned projection.
- Evidence citations outside that projection or revision are invalid.
- Declared agent intent can guide the rubric but cannot prove observed success.
- Human answers, expected outputs, held-out benchmark labels, credentials, and
  raw provider payloads are not exposed through the default read-only MCP
  catalog.

## Local model artifact contract

Perseval can verify and execute an approved task-completion model as an ONNX
artifact without embedding private training code or data in this repository.
An artifact directory contains `manifest.json`, the hashed ONNX model, an
optional hashed tokenizer, and hashed Python/ONNX parity fixtures. The manifest
pins the projector, training-record and 39-feature contracts, base-model
revision, dataset hash, training version, and calibration version.

The Rust runtime rejects path traversal, missing or altered files, incompatible
trace contracts, mismatched tensor names, non-finite logits, and stale
calibration metadata. It applies the versioned temperature and threshold after
ONNX inference. These checks can be run without opening a benchmark holdout:

```text
cargo run -p perseval-model-runtime --bin perseval-model -- verify ARTIFACT_DIRECTORY
cargo run -p perseval-model-runtime --bin perseval-model -- parity ARTIFACT_DIRECTORY
```

Sealed compact projections can be converted to label-free, revision-bound
training records through the reusable `traces-to-evals` contract:

```text
cargo run -p perseval-model-runtime --bin perseval-model -- training-records PROJECTIONS.jsonl RECORDS.jsonl
```

Source identities, split assignments, labels, teacher prompts, raw traces,
training jobs, diagnostics, and unreleased checkpoints remain outside the
public application repository. No model is enabled by default until its frozen
development runs satisfy the documented quality gates; passing artifact parity
is necessary but does not establish model quality.

## Current scope

This milestone ships Task Completion, evidence inspection, human review,
calibration, and the verified local ONNX runtime boundary. It does not ship an
approved local model, hallucination or the other evaluator families,
learned failure discovery, active learning, regression test-set creation,
release experiments, live learned evaluation, or MCP execution of quality
checks. The current Arize comparison is a frozen engineering baseline; it does
not authorize public quality, trace-viewer, or scale superiority claims.
