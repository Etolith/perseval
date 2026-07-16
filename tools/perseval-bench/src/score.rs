use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use perseval_store::{AnalysisStatus, WorkspaceStore, WorkspaceStoreLayout};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HeldOutLabel {
    pub trace_id: String,
    pub instance_id: String,
    pub trajectory_id: String,
    pub resolved: bool,
    pub model: String,
    #[serde(default)]
    pub group_key: Option<String>,
    #[serde(default)]
    pub split: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScoreReport {
    pub schema_version: &'static str,
    pub workspace: String,
    pub source_id: String,
    pub traces: u64,
    pub resolved: u64,
    pub unresolved: u64,
    pub total_findings: u64,
    pub detector_versions: BTreeMap<String, BTreeSet<String>>,
    pub signals: BTreeMap<String, BinaryMetrics>,
    pub classification_errors: BTreeMap<String, Vec<ClassificationError>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClassificationError {
    pub benchmark_trace_id: String,
    pub logical_trace_id: String,
    pub instance_id: String,
    pub trajectory_id: String,
    pub kind: ClassificationErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationErrorKind {
    FalsePositive,
    FalseNegative,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BinaryMetrics {
    pub true_positive: u64,
    pub false_positive: u64,
    pub true_negative: u64,
    pub false_negative: u64,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
    pub accuracy: f64,
    pub false_positive_rate: Option<f64>,
}

pub fn score_workspace(
    workspace: &Path,
    labels_path: &Path,
    source_id: &str,
) -> Result<ScoreReport, Box<dyn Error>> {
    let labels = load_labels(labels_path)?;
    let layout = WorkspaceStoreLayout::new(workspace);
    let store = WorkspaceStore::open(&layout, "default")?;
    let runs = store.list_runs(0, u32::MAX)?;
    let run_status = runs
        .into_iter()
        .map(|run| (run.logical_trace_id, run.analysis_status))
        .collect::<BTreeMap<_, _>>();
    let findings = store.active_findings()?;
    let mut by_trace = BTreeMap::<String, Vec<_>>::new();
    let mut versions = store.active_detector_versions()?;
    for finding in &findings {
        versions
            .entry(finding.detector_id.clone())
            .or_default()
            .insert(finding.detector_version.clone());
        by_trace
            .entry(finding.trace_id.clone())
            .or_default()
            .push(finding);
    }

    let mut examples = Vec::with_capacity(labels.len());
    for label in labels {
        let logical_trace_id = logical_trace_id(source_id, &label.trace_id);
        let Some(status) = run_status.get(&logical_trace_id) else {
            return Err(format!(
                "held-out trace {} is absent from workspace {}",
                label.trace_id,
                workspace.display()
            )
            .into());
        };
        if *status != AnalysisStatus::Ready {
            return Err(format!(
                "held-out trace {} is not analysis-ready: {status:?}",
                label.trace_id
            )
            .into());
        }
        examples.push((label, logical_trace_id));
    }

    let mut signals = BTreeMap::new();
    let mut classification_errors = BTreeMap::new();
    let detector_ids = versions.keys().cloned().collect::<Vec<_>>();
    for detector_id in detector_ids {
        let (metrics, errors) = score_signal(&examples, |trace_id| {
            by_trace.get(trace_id).is_some_and(|findings| {
                findings
                    .iter()
                    .any(|finding| finding.detector_id == detector_id)
            })
        });
        signals.insert(detector_id.clone(), metrics);
        classification_errors.insert(detector_id, errors);
    }
    let (metrics, errors) = score_signal(&examples, |trace_id| {
        by_trace
            .get(trace_id)
            .is_some_and(|items| !items.is_empty())
    });
    signals.insert("any_finding".into(), metrics);
    classification_errors.insert("any_finding".into(), errors);
    for threshold in 2..=6 {
        let signal = format!("finding_count_at_least_{threshold}");
        let (metrics, errors) = score_signal(&examples, |trace_id| {
            by_trace.get(trace_id).map_or(0, Vec::len) >= threshold
        });
        signals.insert(signal.clone(), metrics);
        classification_errors.insert(signal, errors);
    }

    let resolved = examples.iter().filter(|(label, _)| label.resolved).count() as u64;
    Ok(ScoreReport {
        schema_version: "perseval.benchmark_score_report.v1",
        workspace: workspace.display().to_string(),
        source_id: source_id.into(),
        traces: examples.len() as u64,
        resolved,
        unresolved: examples.len() as u64 - resolved,
        total_findings: findings.len() as u64,
        detector_versions: versions,
        signals,
        classification_errors,
    })
}

pub fn write_json_report<T: Serialize>(report: &T, output: &Path) -> Result<(), Box<dyn Error>> {
    let parent = output
        .parent()
        .ok_or_else(|| format!("output path has no parent: {}", output.display()))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.partial-{}",
        output
            .file_name()
            .map(|value| value.to_string_lossy())
            .unwrap_or_default(),
        std::process::id()
    ));
    let mut writer = BufWriter::new(File::create(&temporary)?);
    serde_json::to_writer_pretty(&mut writer, report)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);
    std::fs::rename(temporary, output)?;
    Ok(())
}

pub(crate) fn load_labels(path: &Path) -> Result<Vec<HeldOutLabel>, Box<dyn Error>> {
    let mut labels = Vec::new();
    let mut trace_ids = BTreeSet::new();
    for (line_index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let label: HeldOutLabel = serde_json::from_str(&line).map_err(|error| {
            format!(
                "invalid label JSON on line {} of {}: {error}",
                line_index + 1,
                path.display()
            )
        })?;
        if !trace_ids.insert(label.trace_id.clone()) {
            return Err(format!("duplicate held-out trace label: {}", label.trace_id).into());
        }
        labels.push(label);
    }
    if labels.is_empty() {
        return Err(format!("label sidecar contains no labels: {}", path.display()).into());
    }
    Ok(labels)
}

fn score_signal(
    examples: &[(HeldOutLabel, String)],
    predicted_failure: impl Fn(&str) -> bool,
) -> (BinaryMetrics, Vec<ClassificationError>) {
    let mut result = BinaryMetrics {
        true_positive: 0,
        false_positive: 0,
        true_negative: 0,
        false_negative: 0,
        precision: None,
        recall: None,
        accuracy: 0.0,
        false_positive_rate: None,
    };
    let mut errors = Vec::new();
    for (label, trace_id) in examples {
        let actually_failed = !label.resolved;
        let predicted_failure = predicted_failure(trace_id);
        match (predicted_failure, actually_failed) {
            (true, true) => result.true_positive += 1,
            (true, false) => {
                result.false_positive += 1;
                errors.push(classification_error(
                    label,
                    trace_id,
                    ClassificationErrorKind::FalsePositive,
                ));
            }
            (false, false) => result.true_negative += 1,
            (false, true) => {
                result.false_negative += 1;
                errors.push(classification_error(
                    label,
                    trace_id,
                    ClassificationErrorKind::FalseNegative,
                ));
            }
        }
    }
    result.precision = ratio(
        result.true_positive,
        result.true_positive + result.false_positive,
    );
    result.recall = ratio(
        result.true_positive,
        result.true_positive + result.false_negative,
    );
    result.false_positive_rate = ratio(
        result.false_positive,
        result.false_positive + result.true_negative,
    );
    result.accuracy = ratio(
        result.true_positive + result.true_negative,
        examples.len() as u64,
    )
    .unwrap_or_default();
    (result, errors)
}

fn classification_error(
    label: &HeldOutLabel,
    logical_trace_id: &str,
    kind: ClassificationErrorKind,
) -> ClassificationError {
    ClassificationError {
        benchmark_trace_id: label.trace_id.clone(),
        logical_trace_id: logical_trace_id.into(),
        instance_id: label.instance_id.clone(),
        trajectory_id: label.trajectory_id.clone(),
        kind,
    }
}

fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator != 0).then_some(numerator as f64 / denominator as f64)
}

fn logical_trace_id(source_id: &str, benchmark_trace_id: &str) -> String {
    let external_trace_id = hex::encode(&Sha256::digest(benchmark_trace_id.as_bytes())[..16]);
    hex::encode(Sha256::digest(format!("{source_id}:{external_trace_id}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reproduces_perseval_logical_trace_identity() {
        assert_eq!(
            logical_trace_id("otlp-local", "swesmith-003079c48b2e4f19a2aba236"),
            "110acde8fb78c3dc0867950795ccdeac9602fce1f464e24494d5a3cd9e47032e"
        );
    }

    #[test]
    fn reports_binary_metrics_without_dividing_by_zero() {
        let examples = vec![
            (
                HeldOutLabel {
                    trace_id: "a".into(),
                    instance_id: "a".into(),
                    trajectory_id: "a".into(),
                    resolved: false,
                    model: "m".into(),
                    group_key: None,
                    split: None,
                },
                "a".into(),
            ),
            (
                HeldOutLabel {
                    trace_id: "b".into(),
                    instance_id: "b".into(),
                    trajectory_id: "b".into(),
                    resolved: true,
                    model: "m".into(),
                    group_key: None,
                    split: None,
                },
                "b".into(),
            ),
        ];

        let (metrics, errors) = score_signal(&examples, |trace_id| trace_id == "a");

        assert_eq!(metrics.true_positive, 1);
        assert_eq!(metrics.true_negative, 1);
        assert_eq!(metrics.precision, Some(1.0));
        assert_eq!(metrics.accuracy, 1.0);
        assert!(errors.is_empty());
    }
}
