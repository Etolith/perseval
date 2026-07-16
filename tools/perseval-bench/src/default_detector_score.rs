use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::{Deserialize, Serialize};
use traces_to_evals::{
    AgentBehaviorTrace, DetectorEvaluationStatusV1, DetectorProfileIdentityV1, DetectorProfileV1,
    DeterministicDetectorSet,
};

use crate::detector_score::{
    DetectorMetrics, RateEstimate, UnresolvedEvidenceReference, collect_unresolved_evidence,
    evidence_identities, rate_estimate,
};
use crate::fetch::sha256_file;
use crate::guard::guard_behavior_fixture;

const LABEL_SCHEMA_VERSION: &str = "perseval.detector_expectation.v1";
const REPORT_SCHEMA_VERSION: &str = "perseval.default_detector_qualification.v1";
const MIN_POSITIVE_LABELS: u64 = 24;
const MIN_NEGATIVE_LABELS: u64 = 40;
const MIN_PRECISION: f64 = 0.80;
const MAX_FALSE_POSITIVE_RATE: f64 = 0.10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectorExpectationV1 {
    Finding,
    NoFinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DetectorExpectationLabelV1 {
    pub schema_version: String,
    pub trace_id: String,
    pub repository: String,
    pub task: String,
    pub group_key: String,
    pub split: String,
    pub expectations: BTreeMap<String, DetectorExpectationV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DefaultDetectorQualificationReport {
    pub schema_version: &'static str,
    pub corpus_kind: &'static str,
    pub external_validity: &'static str,
    pub fixture: String,
    pub fixture_sha256: String,
    pub labels: String,
    pub labels_sha256: String,
    pub split: String,
    pub label_policy: &'static str,
    pub traces: u64,
    pub repository_task_groups: u64,
    pub cross_split_group_leaks: u64,
    pub profile: DetectorProfileIdentityV1,
    pub detector_versions: BTreeMap<String, String>,
    pub qualification_policy: QualificationPolicyV1,
    pub detectors: BTreeMap<String, DetectorMetrics>,
    pub all_default_detectors_qualified: bool,
    pub unresolved_evidence_references: u64,
    pub unresolved_evidence_examples: Vec<UnresolvedEvidenceReference>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QualificationPolicyV1 {
    pub minimum_positive_labels: u64,
    pub minimum_negative_labels: u64,
    pub minimum_precision: f64,
    pub maximum_false_positive_rate: f64,
    pub require_precision_wilson_95_low: f64,
    pub require_false_positive_rate_wilson_95_high: f64,
    pub maximum_abstentions: u64,
    pub maximum_unresolved_evidence_references: u64,
}

#[derive(Default)]
struct DetectorAccumulator {
    true_positive: u64,
    false_positive: u64,
    true_negative: u64,
    false_negative: u64,
    positive_labels: u64,
    negative_labels: u64,
    findings: u64,
    traces_with_findings: u64,
    evaluated: u64,
    inconclusive: u64,
    unresolved_evidence_references: u64,
}

pub fn score_default_detectors(
    fixture: &Path,
    labels_path: &Path,
    split: &str,
) -> Result<DefaultDetectorQualificationReport, Box<dyn Error>> {
    guard_behavior_fixture(fixture)?;
    let labels = load_detector_labels(labels_path)?;
    let leaks = cross_split_group_leaks(&labels);
    if leaks != 0 {
        return Err(format!(
            "repository/task groups cross held-out splits: {leaks} leaking groups"
        )
        .into());
    }

    let selected = labels
        .iter()
        .filter(|label| label.split == split)
        .map(|label| (label.trace_id.clone(), label))
        .collect::<BTreeMap<_, _>>();
    if selected.is_empty() {
        return Err(format!("label sidecar has no records for split {split:?}").into());
    }

    let profile = DetectorProfileV1::conservative();
    let detectors = DeterministicDetectorSet::from_profile(profile)?;
    let detector_versions = detectors.detector_versions();
    let default_detector_ids = detector_versions.keys().cloned().collect::<BTreeSet<_>>();
    validate_expectation_coverage(&selected, &default_detector_ids)?;

    let mut accumulators = default_detector_ids
        .iter()
        .map(|detector_id| (detector_id.clone(), DetectorAccumulator::default()))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let mut unresolved_evidence_references = 0_u64;
    let mut unresolved_evidence_examples = Vec::new();

    for (line_index, line) in BufReader::new(File::open(fixture)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let trace: AgentBehaviorTrace = serde_json::from_str(&line).map_err(|error| {
            format!(
                "invalid behavior trace JSON on line {} of {}: {error}",
                line_index + 1,
                fixture.display()
            )
        })?;
        let Some(label) = selected.get(&trace.trace_id) else {
            continue;
        };
        if !seen.insert(trace.trace_id.clone()) {
            return Err(format!("duplicate behavior trace in fixture: {}", trace.trace_id).into());
        }

        let report = detectors.detect_report(&trace);
        let available_evidence = evidence_identities(&trace);
        let finding_ids = report
            .findings
            .iter()
            .map(|finding| finding.detector_id.as_str())
            .collect::<BTreeSet<_>>();
        let coverage = report
            .detector_coverage
            .iter()
            .map(|(detector_id, coverage)| (detector_id.as_str(), coverage.status))
            .collect::<BTreeMap<_, _>>();

        for (detector_id, expectation) in &label.expectations {
            let accumulator = accumulators
                .get_mut(detector_id)
                .expect("validated detector expectation");
            let predicted = finding_ids.contains(detector_id.as_str());
            match expectation {
                DetectorExpectationV1::Finding => {
                    accumulator.positive_labels += 1;
                    if predicted {
                        accumulator.true_positive += 1;
                    } else {
                        accumulator.false_negative += 1;
                    }
                }
                DetectorExpectationV1::NoFinding => {
                    accumulator.negative_labels += 1;
                    if predicted {
                        accumulator.false_positive += 1;
                    } else {
                        accumulator.true_negative += 1;
                    }
                }
            }
            match coverage.get(detector_id.as_str()) {
                Some(DetectorEvaluationStatusV1::Evaluated) => accumulator.evaluated += 1,
                Some(DetectorEvaluationStatusV1::Inconclusive) => accumulator.inconclusive += 1,
                Some(DetectorEvaluationStatusV1::Disabled) | None => {
                    return Err(format!(
                        "default detector {detector_id} was disabled or missing for trace {}",
                        trace.trace_id
                    )
                    .into());
                }
            }
        }

        for finding in &report.findings {
            let Some(accumulator) = accumulators.get_mut(&finding.detector_id) else {
                continue;
            };
            accumulator.findings += 1;
            accumulator.traces_with_findings += 1;
            let before = unresolved_evidence_references;
            collect_unresolved_evidence(
                finding,
                &available_evidence,
                &mut unresolved_evidence_references,
                &mut unresolved_evidence_examples,
            );
            accumulator.unresolved_evidence_references +=
                unresolved_evidence_references.saturating_sub(before);
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

    let scored = accumulators
        .into_iter()
        .map(|(detector_id, accumulator)| {
            let metrics = qualification_metrics(&detector_id, accumulator);
            (detector_id, metrics)
        })
        .collect::<BTreeMap<_, _>>();
    let all_default_detectors_qualified =
        scored.values().all(|metrics| metrics.qualified_for_default);
    let repository_task_groups = selected
        .values()
        .map(|label| label.group_key.as_str())
        .collect::<BTreeSet<_>>()
        .len() as u64;

    Ok(DefaultDetectorQualificationReport {
        schema_version: REPORT_SCHEMA_VERSION,
        corpus_kind: "synthetic_adversarial_contract_suite",
        external_validity: "qualifies deterministic detector semantics and counterexamples; it does not estimate production prevalence or substitute for independently collected domain labels",
        fixture: fixture.display().to_string(),
        fixture_sha256: sha256_file(fixture)?,
        labels: labels_path.display().to_string(),
        labels_sha256: sha256_file(labels_path)?,
        split: split.into(),
        label_policy: "detector-specific semantic expectations are sidecar-only; repository/task groups never cross splits",
        traces: selected.len() as u64,
        repository_task_groups,
        cross_split_group_leaks: leaks,
        profile: detectors.profile().clone(),
        detector_versions,
        qualification_policy: qualification_policy(),
        detectors: scored,
        all_default_detectors_qualified,
        unresolved_evidence_references,
        unresolved_evidence_examples,
    })
}

fn qualification_metrics(detector_id: &str, accumulator: DetectorAccumulator) -> DetectorMetrics {
    let precision = rate_estimate(
        accumulator.true_positive,
        accumulator.true_positive + accumulator.false_positive,
    );
    let recall = rate_estimate(
        accumulator.true_positive,
        accumulator.true_positive + accumulator.false_negative,
    );
    let false_positive_rate = rate_estimate(
        accumulator.false_positive,
        accumulator.false_positive + accumulator.true_negative,
    );
    let qualified = accumulator.positive_labels >= MIN_POSITIVE_LABELS
        && accumulator.negative_labels >= MIN_NEGATIVE_LABELS
        && rate_at_least(&precision, MIN_PRECISION)
        && wilson_low_at_least(&precision, MIN_PRECISION)
        && rate_at_most(&false_positive_rate, MAX_FALSE_POSITIVE_RATE)
        && wilson_high_at_most(&false_positive_rate, MAX_FALSE_POSITIVE_RATE)
        && accumulator.inconclusive == 0
        && accumulator.unresolved_evidence_references == 0;
    let reason = if qualified {
        "meets sample, point-estimate, Wilson-bound, coverage, and evidence gates".into()
    } else {
        format!(
            "requires >= {MIN_POSITIVE_LABELS} positive and >= {MIN_NEGATIVE_LABELS} negative labels, precision and Wilson low >= {:.0}%, false-positive rate and Wilson high <= {:.0}%, zero abstentions, and resolvable evidence (observed positive={}, negative={}, inconclusive={}, unresolved_evidence={})",
            MIN_PRECISION * 100.0,
            MAX_FALSE_POSITIVE_RATE * 100.0,
            accumulator.positive_labels,
            accumulator.negative_labels,
            accumulator.inconclusive,
            accumulator.unresolved_evidence_references,
        )
    };
    DetectorMetrics {
        true_positive: accumulator.true_positive,
        false_positive: accumulator.false_positive,
        true_negative: accumulator.true_negative,
        false_negative: accumulator.false_negative,
        findings: accumulator.findings,
        traces_with_findings: accumulator.traces_with_findings,
        evaluated: accumulator.evaluated,
        inconclusive: accumulator.inconclusive,
        precision,
        recall,
        false_positive_rate,
        qualified_for_default: qualified,
        qualification_reason: format!("{detector_id}: {reason}"),
    }
}

fn qualification_policy() -> QualificationPolicyV1 {
    QualificationPolicyV1 {
        minimum_positive_labels: MIN_POSITIVE_LABELS,
        minimum_negative_labels: MIN_NEGATIVE_LABELS,
        minimum_precision: MIN_PRECISION,
        maximum_false_positive_rate: MAX_FALSE_POSITIVE_RATE,
        require_precision_wilson_95_low: MIN_PRECISION,
        require_false_positive_rate_wilson_95_high: MAX_FALSE_POSITIVE_RATE,
        maximum_abstentions: 0,
        maximum_unresolved_evidence_references: 0,
    }
}

fn rate_at_least(estimate: &RateEstimate, threshold: f64) -> bool {
    estimate.value.is_some_and(|value| value >= threshold)
}

fn rate_at_most(estimate: &RateEstimate, threshold: f64) -> bool {
    estimate.value.is_some_and(|value| value <= threshold)
}

fn wilson_low_at_least(estimate: &RateEstimate, threshold: f64) -> bool {
    estimate
        .wilson_95_low
        .is_some_and(|value| value >= threshold)
}

fn wilson_high_at_most(estimate: &RateEstimate, threshold: f64) -> bool {
    estimate
        .wilson_95_high
        .is_some_and(|value| value <= threshold)
}

fn load_detector_labels(path: &Path) -> Result<Vec<DetectorExpectationLabelV1>, Box<dyn Error>> {
    let mut labels = Vec::new();
    let mut trace_ids = BTreeSet::new();
    for (line_index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let label: DetectorExpectationLabelV1 = serde_json::from_str(&line).map_err(|error| {
            format!(
                "invalid detector label JSON on line {} of {}: {error}",
                line_index + 1,
                path.display()
            )
        })?;
        if label.schema_version != LABEL_SCHEMA_VERSION {
            return Err(format!(
                "unsupported detector label schema {:?} for trace {}",
                label.schema_version, label.trace_id
            )
            .into());
        }
        if label.trace_id.trim().is_empty()
            || label.repository.trim().is_empty()
            || label.task.trim().is_empty()
            || label.group_key.trim().is_empty()
            || label.split.trim().is_empty()
            || label.expectations.is_empty()
        {
            return Err(format!("incomplete detector label for trace {}", label.trace_id).into());
        }
        if !trace_ids.insert(label.trace_id.clone()) {
            return Err(format!("duplicate detector label: {}", label.trace_id).into());
        }
        labels.push(label);
    }
    if labels.is_empty() {
        return Err(format!("detector label sidecar is empty: {}", path.display()).into());
    }
    Ok(labels)
}

fn validate_expectation_coverage(
    labels: &BTreeMap<String, &DetectorExpectationLabelV1>,
    default_detector_ids: &BTreeSet<String>,
) -> Result<(), Box<dyn Error>> {
    let mut mentioned = BTreeSet::new();
    for label in labels.values() {
        for detector_id in label.expectations.keys() {
            if !default_detector_ids.contains(detector_id) {
                return Err(format!(
                    "trace {} labels non-default detector {detector_id:?}",
                    label.trace_id
                )
                .into());
            }
            mentioned.insert(detector_id.clone());
        }
    }
    if &mentioned != default_detector_ids {
        let missing = default_detector_ids
            .difference(&mentioned)
            .cloned()
            .collect::<Vec<_>>();
        return Err(format!(
            "split does not label every default detector; missing: {}",
            missing.join(", ")
        )
        .into());
    }
    Ok(())
}

fn cross_split_group_leaks(labels: &[DetectorExpectationLabelV1]) -> u64 {
    let mut splits_by_group = BTreeMap::<&str, BTreeSet<&str>>::new();
    for label in labels {
        splits_by_group
            .entry(label.group_key.as_str())
            .or_default()
            .insert(label.split.as_str());
    }
    splits_by_group
        .values()
        .filter(|splits| splits.len() > 1)
        .count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_policy_requires_wilson_supported_sample_sizes() {
        let too_small = qualification_metrics(
            "detector",
            DetectorAccumulator {
                true_positive: 20,
                true_negative: 20,
                positive_labels: 20,
                negative_labels: 20,
                evaluated: 40,
                ..DetectorAccumulator::default()
            },
        );
        assert!(!too_small.qualified_for_default);

        let supported = qualification_metrics(
            "detector",
            DetectorAccumulator {
                true_positive: 24,
                true_negative: 40,
                positive_labels: 24,
                negative_labels: 40,
                evaluated: 64,
                ..DetectorAccumulator::default()
            },
        );
        assert!(supported.qualified_for_default);
    }
}
