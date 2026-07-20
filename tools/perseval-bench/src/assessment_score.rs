use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use perseval_store::{AssessmentItemStatusV1, AssessmentJobExportV1};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use traces_to_evals::LearnedVerdictV1;

#[derive(Debug, Deserialize)]
struct TaskCompletionLabel {
    trace_id: String,
    trajectory_id: String,
    resolved: bool,
    group_key: String,
    split: String,
}

#[derive(Debug, Clone, Copy)]
struct ScoredCase {
    actual_failure: bool,
    predicted_failure: Option<bool>,
    raw_failure_score: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct AssessmentScoreReportV1 {
    schema_version: String,
    export_sha256: String,
    labels_sha256: String,
    evaluator_release_id: String,
    selection_hash: String,
    attempted_export_items: usize,
    label_rows: usize,
    matched_items: usize,
    unmatched_labels: usize,
    decision_count: usize,
    decision_coverage: f64,
    abstained_or_unavailable: usize,
    status_counts: BTreeMap<String, usize>,
    split_counts: BTreeMap<String, usize>,
    independent_groups: usize,
    confusion: BinaryConfusionV1,
    metrics: BinaryMetricsV1,
    total_cost_micros: u64,
    total_latency_ms: u64,
    report_sha256: String,
}

#[derive(Debug, Default, Serialize)]
struct BinaryConfusionV1 {
    true_positive: usize,
    false_positive: usize,
    true_negative: usize,
    false_negative: usize,
}

#[derive(Debug, Serialize)]
struct BinaryMetricsV1 {
    positive_class: &'static str,
    precision: Option<f64>,
    recall: Option<f64>,
    f1: Option<f64>,
    macro_f1: Option<f64>,
    mcc: Option<f64>,
    raw_auprc: Option<f64>,
    raw_brier: Option<f64>,
    raw_ece_10_equal_width_bins: Option<f64>,
    calibration_state: &'static str,
}

pub fn score_assessments(
    export_path: &Path,
    labels_path: &Path,
) -> Result<AssessmentScoreReportV1, Box<dyn Error>> {
    let export_bytes = std::fs::read(export_path)?;
    let export: AssessmentJobExportV1 = serde_json::from_slice(&export_bytes)?;
    let labels_bytes = std::fs::read(labels_path)?;
    let labels = read_labels(labels_path)?;
    let by_external_id = export
        .items
        .iter()
        .map(|item| (item.external_trace_id.as_str(), item))
        .collect::<HashMap<_, _>>();

    let mut cases = Vec::new();
    let mut status_counts = BTreeMap::new();
    let mut split_counts = BTreeMap::new();
    let mut groups = std::collections::BTreeSet::new();
    let mut total_cost_micros = 0_u64;
    let mut total_latency_ms = 0_u64;
    for label in &labels {
        let derived_external_trace_id = external_trace_id(&label.trace_id);
        let Some(item) = by_external_id
            .get(derived_external_trace_id.as_str())
            .or_else(|| by_external_id.get(label.trajectory_id.as_str()))
            .or_else(|| by_external_id.get(label.trace_id.as_str()))
        else {
            continue;
        };
        *split_counts.entry(label.split.clone()).or_insert(0) += 1;
        groups.insert(label.group_key.clone());
        *status_counts
            .entry(status_name(item.status).to_string())
            .or_insert(0) += 1;
        if let Some(assessment) = &item.assessment {
            total_cost_micros = total_cost_micros.saturating_add(assessment.cost_micros);
            total_latency_ms = total_latency_ms.saturating_add(assessment.latency_ms);
        }
        let evaluation = item
            .assessment
            .as_ref()
            .and_then(|assessment| assessment.evaluation.as_ref());
        let predicted_failure = evaluation.and_then(|evaluation| match evaluation.verdict {
            LearnedVerdictV1::Pass => Some(false),
            LearnedVerdictV1::Fail => Some(true),
            LearnedVerdictV1::Abstain => None,
        });
        let raw_failure_score = evaluation
            .and_then(|evaluation| evaluation.score)
            .map(|completion_score| 1.0 - completion_score);
        cases.push(ScoredCase {
            actual_failure: !label.resolved,
            predicted_failure,
            raw_failure_score,
        });
    }

    let confusion = confusion(&cases);
    let metrics = metrics(&cases, &confusion);
    let decision_count = cases
        .iter()
        .filter(|case| case.predicted_failure.is_some())
        .count();
    let mut report = AssessmentScoreReportV1 {
        schema_version: "perseval.assessment_score_report.v1".into(),
        export_sha256: sha256(&export_bytes),
        labels_sha256: sha256(&labels_bytes),
        evaluator_release_id: export.job.evaluator_release_id.clone(),
        selection_hash: export.job.selection_hash.clone(),
        attempted_export_items: export.items.len(),
        label_rows: labels.len(),
        matched_items: cases.len(),
        unmatched_labels: labels.len().saturating_sub(cases.len()),
        decision_count,
        decision_coverage: ratio(decision_count, cases.len()).unwrap_or(0.0),
        abstained_or_unavailable: cases.len().saturating_sub(decision_count),
        status_counts,
        split_counts,
        independent_groups: groups.len(),
        confusion,
        metrics,
        total_cost_micros,
        total_latency_ms,
        report_sha256: String::new(),
    };
    report.report_sha256 = sha256(&serde_json::to_vec(&report)?);
    Ok(report)
}

fn read_labels(path: &Path) -> Result<Vec<TaskCompletionLabel>, Box<dyn Error>> {
    let mut labels = Vec::new();
    for (index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let label: TaskCompletionLabel = serde_json::from_str(&line)
            .map_err(|error| format!("invalid label row {}: {error}", index + 1))?;
        labels.push(label);
    }
    Ok(labels)
}

fn confusion(cases: &[ScoredCase]) -> BinaryConfusionV1 {
    let mut result = BinaryConfusionV1::default();
    for case in cases {
        match (case.predicted_failure, case.actual_failure) {
            (Some(true), true) => result.true_positive += 1,
            (Some(true), false) => result.false_positive += 1,
            (Some(false), false) => result.true_negative += 1,
            (Some(false), true) => result.false_negative += 1,
            (None, _) => {}
        }
    }
    result
}

fn metrics(cases: &[ScoredCase], confusion: &BinaryConfusionV1) -> BinaryMetricsV1 {
    let precision = ratio(
        confusion.true_positive,
        confusion.true_positive + confusion.false_positive,
    );
    let recall = ratio(
        confusion.true_positive,
        confusion.true_positive + confusion.false_negative,
    );
    let f1 = harmonic_mean(precision, recall);
    let negative_precision = ratio(
        confusion.true_negative,
        confusion.true_negative + confusion.false_negative,
    );
    let negative_recall = ratio(
        confusion.true_negative,
        confusion.true_negative + confusion.false_positive,
    );
    let negative_f1 = harmonic_mean(negative_precision, negative_recall);
    let denominator = ((confusion.true_positive + confusion.false_positive) as f64
        * (confusion.true_positive + confusion.false_negative) as f64
        * (confusion.true_negative + confusion.false_positive) as f64
        * (confusion.true_negative + confusion.false_negative) as f64)
        .sqrt();
    let mcc = (denominator > 0.0).then(|| {
        (confusion.true_positive as f64 * confusion.true_negative as f64
            - confusion.false_positive as f64 * confusion.false_negative as f64)
            / denominator
    });
    let scored = cases
        .iter()
        .filter_map(|case| {
            case.raw_failure_score
                .map(|score| (score.clamp(0.0, 1.0), case.actual_failure))
        })
        .collect::<Vec<_>>();
    BinaryMetricsV1 {
        positive_class: "task_failure_or_partial",
        precision,
        recall,
        f1,
        macro_f1: f1
            .zip(negative_f1)
            .map(|(positive, negative)| (positive + negative) / 2.0),
        mcc,
        raw_auprc: average_precision(&scored),
        raw_brier: (!scored.is_empty()).then(|| {
            scored
                .iter()
                .map(|(score, actual)| {
                    let target = if *actual { 1.0 } else { 0.0 };
                    (score - target).powi(2)
                })
                .sum::<f64>()
                / scored.len() as f64
        }),
        raw_ece_10_equal_width_bins: ece(&scored, 10),
        calibration_state: "raw_completion_score_inverted; not calibrated",
    }
}

fn average_precision(scored: &[(f64, bool)]) -> Option<f64> {
    let positives = scored.iter().filter(|(_, actual)| *actual).count();
    if positives == 0 {
        return None;
    }
    let mut ranked = scored.to_vec();
    ranked.sort_by(|left, right| right.0.partial_cmp(&left.0).unwrap_or(Ordering::Equal));
    let mut true_positives = 0_usize;
    let mut processed = 0_usize;
    let mut previous_recall = 0.0;
    let mut area = 0.0;
    while processed < ranked.len() {
        let score = ranked[processed].0;
        let mut end = processed;
        while end < ranked.len() && ranked[end].0 == score {
            if ranked[end].1 {
                true_positives += 1;
            }
            end += 1;
        }
        let recall = true_positives as f64 / positives as f64;
        let precision = true_positives as f64 / end as f64;
        area += (recall - previous_recall) * precision;
        previous_recall = recall;
        processed = end;
    }
    Some(area)
}

fn ece(scored: &[(f64, bool)], bins: usize) -> Option<f64> {
    if scored.is_empty() || bins == 0 {
        return None;
    }
    let mut counts = vec![0_usize; bins];
    let mut confidence_sum = vec![0.0; bins];
    let mut actual_sum = vec![0.0; bins];
    for (score, actual) in scored {
        let index = ((*score * bins as f64).floor() as usize).min(bins - 1);
        counts[index] += 1;
        confidence_sum[index] += *score;
        actual_sum[index] += if *actual { 1.0 } else { 0.0 };
    }
    Some(
        counts
            .iter()
            .enumerate()
            .filter(|(_, count)| **count > 0)
            .map(|(index, count)| {
                let count = *count as f64;
                (confidence_sum[index] / count - actual_sum[index] / count).abs() * count
                    / scored.len() as f64
            })
            .sum(),
    )
}

fn harmonic_mean(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    left.zip(right).map(|(left, right)| {
        if left + right == 0.0 {
            0.0
        } else {
            2.0 * left * right / (left + right)
        }
    })
}

fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn external_trace_id(benchmark_trace_id: &str) -> String {
    let digest = Sha256::digest(benchmark_trace_id.as_bytes());
    hex::encode(&digest[..16])
}

fn status_name(status: AssessmentItemStatusV1) -> &'static str {
    use AssessmentItemStatusV1::*;
    match status {
        Pending => "pending",
        Running => "running",
        Succeeded => "succeeded",
        Abstained => "abstained",
        Failed => "failed",
        Cancelled => "cancelled",
        BudgetBlocked => "budget_blocked",
        PrivacyBlocked => "privacy_blocked",
        ProviderUnavailable => "provider_unavailable",
        NotApplicable => "not_applicable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_confusion_metrics_match_exact_values() {
        let cases = [
            ScoredCase {
                actual_failure: true,
                predicted_failure: Some(true),
                raw_failure_score: Some(0.9),
            },
            ScoredCase {
                actual_failure: false,
                predicted_failure: Some(true),
                raw_failure_score: Some(0.8),
            },
            ScoredCase {
                actual_failure: false,
                predicted_failure: Some(false),
                raw_failure_score: Some(0.1),
            },
            ScoredCase {
                actual_failure: true,
                predicted_failure: Some(false),
                raw_failure_score: Some(0.2),
            },
            ScoredCase {
                actual_failure: true,
                predicted_failure: None,
                raw_failure_score: None,
            },
        ];
        let confusion = confusion(&cases);
        assert_eq!(confusion.true_positive, 1);
        assert_eq!(confusion.false_positive, 1);
        assert_eq!(confusion.true_negative, 1);
        assert_eq!(confusion.false_negative, 1);
        let metrics = metrics(&cases, &confusion);
        assert_eq!(metrics.precision, Some(0.5));
        assert_eq!(metrics.recall, Some(0.5));
        assert_eq!(metrics.f1, Some(0.5));
        assert_eq!(metrics.macro_f1, Some(0.5));
        assert_eq!(metrics.mcc, Some(0.0));
    }

    #[test]
    fn average_precision_is_tie_aware() {
        let scored = [(0.9, true), (0.8, false), (0.8, true), (0.1, false)];
        let score = average_precision(&scored).unwrap();
        assert!((score - (1.0 / 2.0 + 0.5 * 2.0 / 3.0)).abs() < 1e-12);
    }

    #[test]
    fn benchmark_trace_identity_matches_imported_external_trace_identity() {
        assert_eq!(
            external_trace_id("linuxarena:783799e77fbcc6379d8e65816a75ae39"),
            "c09f35e78e9e02e1d7a33fccc422081b"
        );
    }
}
