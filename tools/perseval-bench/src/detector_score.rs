use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::Serialize;
use traces_to_evals::{
    AgentBehaviorNormalizer, AgentBehaviorTrace, BehaviorFinding, DetectorEvaluationStatusV1,
    DetectorProfileIdentityV1, DetectorProfileV1, DeterministicDetectorSet,
    OpenInferenceBehaviorNormalizer, Trace,
};

use crate::fetch::sha256_file;
use crate::guard::guard_fixture;
use crate::score::{HeldOutLabel, load_labels};

const REPEATED_PRECISION_GATE: f64 = 0.75;
const REPEATED_FALSE_POSITIVE_RATE_GATE: f64 = 0.15;
const LOOP_FALSE_POSITIVE_RATE_GATE: f64 = 0.10;
pub(crate) const MAX_UNRESOLVED_EVIDENCE_EXAMPLES: usize = 100;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DetectorQualificationReport {
    pub schema_version: &'static str,
    pub fixture: String,
    pub fixture_sha256: String,
    pub labels: String,
    pub labels_sha256: String,
    pub split: String,
    pub label_policy: &'static str,
    pub traces: u64,
    pub resolved: u64,
    pub unresolved: u64,
    pub repository_task_groups: u64,
    pub cross_split_group_leaks: u64,
    pub profile: DetectorProfileIdentityV1,
    pub detector_versions: BTreeMap<String, String>,
    pub detectors: BTreeMap<String, DetectorMetrics>,
    pub unresolved_evidence_references: u64,
    pub unresolved_evidence_examples: Vec<UnresolvedEvidenceReference>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DetectorMetrics {
    pub true_positive: u64,
    pub false_positive: u64,
    pub true_negative: u64,
    pub false_negative: u64,
    pub findings: u64,
    pub traces_with_findings: u64,
    pub evaluated: u64,
    pub inconclusive: u64,
    pub precision: RateEstimate,
    pub recall: RateEstimate,
    pub false_positive_rate: RateEstimate,
    pub qualified_for_default: bool,
    pub qualification_reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RateEstimate {
    pub value: Option<f64>,
    pub sample_size: u64,
    pub wilson_95_low: Option<f64>,
    pub wilson_95_high: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnresolvedEvidenceReference {
    pub trace_id: String,
    pub finding_id: String,
    pub detector_id: String,
    pub evidence_kind: String,
    pub evidence_identity: String,
}

#[derive(Default)]
struct DetectorAccumulator {
    predicted_trace_ids: BTreeSet<String>,
    findings: u64,
    evaluated: u64,
    inconclusive: u64,
}

pub fn score_detectors(
    fixture: &Path,
    labels_path: &Path,
    split: &str,
) -> Result<DetectorQualificationReport, Box<dyn Error>> {
    guard_fixture(fixture)?;
    let labels = load_labels(labels_path)?;
    let leaks = cross_split_group_leaks(&labels)?;
    if leaks != 0 {
        return Err(format!(
            "repository/task groups cross held-out splits: {leaks} leaking groups"
        )
        .into());
    }
    let selected = labels
        .iter()
        .filter(|label| label.split.as_deref() == Some(split))
        .map(|label| (label.trace_id.clone(), label))
        .collect::<BTreeMap<_, _>>();
    if selected.is_empty() {
        return Err(format!("label sidecar has no records for split {split:?}").into());
    }

    let mut profile = DetectorProfileV1::conservative();
    profile.identity = DetectorProfileIdentityV1 {
        profile_id: "traceeval.qualification.swe_smith_retry_episodes".into(),
        profile_version: "1".into(),
    };
    profile.enabled_detectors = ["repeated_tool_failure", "tool_call_loop"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let detectors = DeterministicDetectorSet::from_profile(profile)?;
    let detector_versions = detectors.detector_versions();
    let mut accumulators = detector_versions
        .keys()
        .map(|detector_id| (detector_id.clone(), DetectorAccumulator::default()))
        .collect::<BTreeMap<_, _>>();
    let normalizer = OpenInferenceBehaviorNormalizer::default();
    let mut seen = BTreeSet::new();
    let mut unresolved_evidence_references = 0_u64;
    let mut unresolved_evidence_examples = Vec::new();

    for (line_index, line) in BufReader::new(File::open(fixture)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let trace: Trace = serde_json::from_str(&line).map_err(|error| {
            format!(
                "invalid trace JSON on line {} of {}: {error}",
                line_index + 1,
                fixture.display()
            )
        })?;
        if !selected.contains_key(&trace.id) {
            continue;
        }
        if !seen.insert(trace.id.clone()) {
            return Err(format!("duplicate trace in fixture: {}", trace.id).into());
        }
        let behavior = normalizer.normalize(&trace)?;
        let available_evidence = evidence_identities(&behavior);
        let report = detectors.detect_report(&behavior);
        for (detector_id, coverage) in &report.detector_coverage {
            let Some(accumulator) = accumulators.get_mut(detector_id) else {
                continue;
            };
            match coverage.status {
                DetectorEvaluationStatusV1::Evaluated => accumulator.evaluated += 1,
                DetectorEvaluationStatusV1::Inconclusive => accumulator.inconclusive += 1,
                DetectorEvaluationStatusV1::Disabled => {}
            }
        }
        for finding in &report.findings {
            let Some(accumulator) = accumulators.get_mut(&finding.detector_id) else {
                continue;
            };
            accumulator.findings += 1;
            accumulator
                .predicted_trace_ids
                .insert(finding.trace_id.clone());
            collect_unresolved_evidence(
                finding,
                &available_evidence,
                &mut unresolved_evidence_references,
                &mut unresolved_evidence_examples,
            );
        }
    }
    if seen.len() != selected.len() {
        let missing = selected
            .keys()
            .filter(|trace_id| !seen.contains(*trace_id))
            .take(10)
            .cloned()
            .collect::<Vec<_>>();
        return Err(format!(
            "fixture is missing {} labeled traces from split {split:?}; examples: {}",
            selected.len() - seen.len(),
            missing.join(", ")
        )
        .into());
    }

    let mut scored = BTreeMap::new();
    for (detector_id, accumulator) in accumulators {
        scored.insert(
            detector_id.clone(),
            score_detector(&detector_id, &selected, accumulator),
        );
    }
    let resolved = selected.values().filter(|label| label.resolved).count() as u64;
    let repository_task_groups = selected
        .values()
        .filter_map(|label| label.group_key.as_deref())
        .collect::<BTreeSet<_>>()
        .len() as u64;
    Ok(DetectorQualificationReport {
        schema_version: "perseval.detector_qualification.v1",
        fixture: fixture.display().to_string(),
        fixture_sha256: sha256_file(fixture)?,
        labels: labels_path.display().to_string(),
        labels_sha256: sha256_file(labels_path)?,
        split: split.into(),
        label_policy: "SWE-Smith unresolved is a coarse task-failure proxy; labels remain sidecar-only",
        traces: selected.len() as u64,
        resolved,
        unresolved: selected.len() as u64 - resolved,
        repository_task_groups,
        cross_split_group_leaks: leaks,
        profile: detectors.profile().clone(),
        detector_versions,
        detectors: scored,
        unresolved_evidence_references,
        unresolved_evidence_examples,
    })
}

fn score_detector(
    detector_id: &str,
    labels: &BTreeMap<String, &HeldOutLabel>,
    accumulator: DetectorAccumulator,
) -> DetectorMetrics {
    let mut true_positive = 0_u64;
    let mut false_positive = 0_u64;
    let mut true_negative = 0_u64;
    let mut false_negative = 0_u64;
    for (trace_id, label) in labels {
        match (
            !label.resolved,
            accumulator.predicted_trace_ids.contains(trace_id),
        ) {
            (true, true) => true_positive += 1,
            (false, true) => false_positive += 1,
            (false, false) => true_negative += 1,
            (true, false) => false_negative += 1,
        }
    }
    let precision = rate_estimate(true_positive, true_positive + false_positive);
    let recall = rate_estimate(true_positive, true_positive + false_negative);
    let false_positive_rate = rate_estimate(false_positive, false_positive + true_negative);
    let evidence_agnostic_gate = match detector_id {
        "repeated_tool_failure" => {
            precision
                .value
                .is_some_and(|value| value >= REPEATED_PRECISION_GATE)
                && false_positive_rate
                    .value
                    .is_some_and(|value| value <= REPEATED_FALSE_POSITIVE_RATE_GATE)
        }
        "tool_call_loop" => {
            false_positive_rate
                .value
                .is_some_and(|value| value <= LOOP_FALSE_POSITIVE_RATE_GATE)
                && precision.sample_size >= 20
        }
        _ => false,
    };
    let qualification_reason = match detector_id {
        "repeated_tool_failure" if evidence_agnostic_gate => {
            "meets held-out precision and false-positive-rate gates".into()
        }
        "repeated_tool_failure" => format!(
            "requires precision >= {:.0}% and false-positive rate <= {:.0}%",
            REPEATED_PRECISION_GATE * 100.0,
            REPEATED_FALSE_POSITIVE_RATE_GATE * 100.0
        ),
        "tool_call_loop" if evidence_agnostic_gate => {
            "meets held-out false-positive-rate gate with at least 20 predictions".into()
        }
        "tool_call_loop" => format!(
            "requires false-positive rate <= {:.0}% and at least 20 predictions",
            LOOP_FALSE_POSITIVE_RATE_GATE * 100.0
        ),
        _ => "no qualification policy is defined for this detector".into(),
    };
    DetectorMetrics {
        true_positive,
        false_positive,
        true_negative,
        false_negative,
        findings: accumulator.findings,
        traces_with_findings: accumulator.predicted_trace_ids.len() as u64,
        evaluated: accumulator.evaluated,
        inconclusive: accumulator.inconclusive,
        precision,
        recall,
        false_positive_rate,
        qualified_for_default: evidence_agnostic_gate,
        qualification_reason,
    }
}

pub(crate) fn rate_estimate(successes: u64, sample_size: u64) -> RateEstimate {
    if sample_size == 0 {
        return RateEstimate {
            value: None,
            sample_size,
            wilson_95_low: None,
            wilson_95_high: None,
        };
    }
    let n = sample_size as f64;
    let value = successes as f64 / n;
    let z = 1.959_963_984_540_054_f64;
    let denominator = 1.0 + z * z / n;
    let center = (value + z * z / (2.0 * n)) / denominator;
    let margin = z * ((value * (1.0 - value) / n + z * z / (4.0 * n * n)).sqrt()) / denominator;
    RateEstimate {
        value: Some(value),
        sample_size,
        wilson_95_low: Some((center - margin).max(0.0)),
        wilson_95_high: Some((center + margin).min(1.0)),
    }
}

pub(crate) fn evidence_identities(trace: &AgentBehaviorTrace) -> BTreeSet<String> {
    trace
        .evidence
        .iter()
        .chain(trace.tool_calls.iter().flat_map(|call| &call.evidence))
        .chain(
            trace
                .policy_decisions
                .iter()
                .flat_map(|decision| &decision.evidence),
        )
        .chain(trace.final_outcome.evidence.iter())
        .chain(
            trace
                .final_outcome
                .claims
                .iter()
                .flat_map(|claim| &claim.evidence),
        )
        .map(|evidence| evidence.identity.clone())
        .collect()
}

pub(crate) fn collect_unresolved_evidence(
    finding: &BehaviorFinding,
    available: &BTreeSet<String>,
    count: &mut u64,
    examples: &mut Vec<UnresolvedEvidenceReference>,
) {
    for evidence in &finding.evidence {
        if available.contains(&evidence.identity) {
            continue;
        }
        *count = count.saturating_add(1);
        if examples.len() < MAX_UNRESOLVED_EVIDENCE_EXAMPLES {
            examples.push(UnresolvedEvidenceReference {
                trace_id: finding.trace_id.clone(),
                finding_id: finding.finding_id.clone(),
                detector_id: finding.detector_id.clone(),
                evidence_kind: evidence.kind.clone(),
                evidence_identity: evidence.identity.clone(),
            });
        }
    }
}

fn cross_split_group_leaks(labels: &[HeldOutLabel]) -> Result<u64, Box<dyn Error>> {
    let mut splits_by_group = BTreeMap::<&str, BTreeSet<&str>>::new();
    for label in labels {
        let group = label
            .group_key
            .as_deref()
            .ok_or("detector qualification requires group_key in every label")?;
        let split = label
            .split
            .as_deref()
            .ok_or("detector qualification requires split in every label")?;
        splits_by_group.entry(group).or_default().insert(split);
    }
    Ok(splits_by_group
        .values()
        .filter(|splits| splits.len() > 1)
        .count() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wilson_interval_is_bounded_and_contains_the_observed_rate() {
        let estimate = rate_estimate(8, 10);
        assert_eq!(estimate.value, Some(0.8));
        assert!(estimate.wilson_95_low.unwrap() < 0.8);
        assert!(estimate.wilson_95_high.unwrap() > 0.8);
        assert!(estimate.wilson_95_low.unwrap() >= 0.0);
        assert!(estimate.wilson_95_high.unwrap() <= 1.0);
    }

    #[test]
    fn empty_rate_is_an_explicit_abstention() {
        assert_eq!(
            rate_estimate(0, 0),
            RateEstimate {
                value: None,
                sample_size: 0,
                wilson_95_low: None,
                wilson_95_high: None,
            }
        );
    }
}
