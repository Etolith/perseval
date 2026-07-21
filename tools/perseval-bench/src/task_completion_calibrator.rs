use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use traces_to_evals::canonical_content_id;

use crate::fetch::sha256_file;
use crate::task_completion_features::{
    FEATURE_NAMES as STRUCTURED_FEATURE_NAMES,
    FEATURE_SET_VERSION as STRUCTURED_FEATURE_SET_VERSION, load_feature_records,
};
use crate::task_completion_models::{
    SEMANTIC_FEATURE_NAMES, SEMANTIC_FEATURE_SET_VERSION, load_semantic_feature_records,
};

const REPORT_SCHEMA_VERSION: &str = "perseval.task_completion_cpu_calibration.v2";
const MODEL_SCHEMA_VERSION: &str = "perseval.task_completion_logistic_model.v1";
const FEATURE_SET_VERSION: &str = "perseval.learned_task_completion_scores.v1";
const SMOLLM_SINGLE_FEATURE_SET_VERSION: &str =
    "perseval.smollm_mandatory_recovery_task_completion_score.v1";
const CLOUD_SINGLE_FEATURE_SET_VERSION: &str = "perseval.cloud_compact_task_completion_score.v1";
const CLOUD_PROMPT_VERSION: &str = "perseval.compact-task-completion-cloud-v1";
const SMOLLM_PROMPT_VERSION: &str = "perseval.binary-task-completion-ab-v1";
const FUSED_FEATURE_SET_VERSION: &str = "perseval.task_completion_structured_semantic_fusion.v1";
const FEATURE_NAMES: [&str; 6] = [
    "smollm_goal_final_logit",
    "smollm_mandatory_logit",
    "smollm_mandatory_recovery_logit",
    "smollm_complete_projection_logit",
    "modernbert_entailment_minus_contradiction",
    "modernbert_neutral_minus_decisive",
];
const ALPHAS: [f64; 5] = [0.001, 0.01, 0.1, 1.0, 10.0];
const SEEDS: [u64; 5] = [17, 29, 43, 71, 101];
const OUTER_FOLDS: usize = 5;
const INNER_FOLDS: usize = 4;
const F1_EXIT: f64 = 0.206;
const MCC_EXIT: f64 = 0.200;

pub struct LearnedResultPaths<'a> {
    pub goal_final: &'a Path,
    pub mandatory: &'a Path,
    pub mandatory_recovery: &'a Path,
    pub complete: &'a Path,
    pub nli: &'a Path,
}

#[derive(Debug, Clone, Deserialize)]
struct ResolutionLabel {
    trace_id: String,
    resolved: bool,
    group_key: String,
    split: String,
    #[serde(default)]
    slice: ResolutionSlice,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ResolutionSlice {
    #[serde(default)]
    source: String,
    #[serde(default)]
    domain_family: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RunRecord {
    target_key: String,
    #[serde(default)]
    mandatory_facts_omitted: u32,
    decision: DecisionRecord,
    #[serde(default)]
    nli_diagnostics: Option<NliDiagnostics>,
}

#[derive(Debug, Clone, Deserialize)]
struct DecisionRecord {
    target_key: String,
    target_revision: String,
    trace_context_binding_id: String,
    raw_logit_difference: Option<f64>,
    inference: InferenceRecord,
}

#[derive(Debug, Clone, Deserialize)]
struct InferenceRecord {
    model_id: String,
    prompt_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct NliDiagnostics {
    logits: NliLogits,
}

#[derive(Debug, Clone, Deserialize)]
struct NliLogits {
    entailment: f64,
    neutral: f64,
    contradiction: f64,
}

#[derive(Debug, Clone)]
struct Sample {
    group_key: String,
    source: String,
    domain_family: String,
    incomplete: bool,
    features: Vec<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CalibrationReport {
    schema_version: String,
    feature_set_version: String,
    training_split: String,
    holdout_evaluated: bool,
    sample_count: usize,
    group_count: usize,
    incomplete_count: usize,
    completed_count: usize,
    feature_names: Vec<String>,
    alpha_candidates: Vec<f64>,
    seeds: Vec<u64>,
    source_sha256: BTreeMap<String, String>,
    nested_out_of_fold: Vec<SeedReport>,
    leave_one_source_out: Vec<CohortReport>,
    leave_one_domain_out: Vec<CohortReport>,
    stability: StabilityReport,
    final_model: LogisticArtifact,
}

#[derive(Debug, Clone, Serialize)]
struct SeedReport {
    seed: u64,
    metrics: Metrics,
    folds: Vec<FoldReport>,
}

#[derive(Debug, Clone, Serialize)]
struct FoldReport {
    fold: usize,
    training_samples: usize,
    validation_samples: usize,
    alpha: f64,
    threshold: f64,
}

#[derive(Debug, Clone, Serialize)]
struct CohortReport {
    held_out: String,
    training_samples: usize,
    validation_samples: usize,
    validation_groups: usize,
    alpha: f64,
    threshold: f64,
    metrics: Metrics,
}

#[derive(Debug, Clone, Serialize)]
struct StabilityReport {
    required_f1_exclusive: f64,
    required_mcc_exclusive: f64,
    min_f1: f64,
    median_f1: f64,
    min_mcc: f64,
    median_mcc: f64,
    max_mcc: f64,
    all_seeds_pass: bool,
    min_leave_source_mcc: Option<f64>,
    min_leave_domain_mcc: Option<f64>,
    all_held_out_cohorts_pass: bool,
    advances_to_frozen_holdout: bool,
    decision: String,
}

#[derive(Debug, Clone, Serialize)]
struct LogisticArtifact {
    schema_version: String,
    model_id: String,
    feature_set_version: String,
    feature_names: Vec<String>,
    fitted_on_split: String,
    fitted_samples: usize,
    alpha: f64,
    threshold: f64,
    means: Vec<f64>,
    scales: Vec<f64>,
    weights: Vec<f64>,
    intercept: f64,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct Metrics {
    true_positive: u64,
    false_positive: u64,
    true_negative: u64,
    false_negative: u64,
    precision: f64,
    recall: f64,
    f1: f64,
    mcc: f64,
    accuracy: f64,
    auroc: f64,
    brier: f64,
    expected_calibration_error: f64,
}

#[derive(Debug, Clone)]
struct Standardizer {
    means: Vec<f64>,
    scales: Vec<f64>,
}

#[derive(Debug, Clone)]
struct LogisticModel {
    weights: Vec<f64>,
    intercept: f64,
}

#[derive(Debug, Clone, Copy)]
struct Hyperparameters {
    alpha: f64,
    threshold: f64,
}

pub fn calibrate(
    labels_path: &Path,
    split: &str,
    paths: LearnedResultPaths<'_>,
) -> Result<CalibrationReport> {
    anyhow::ensure!(
        split == "baseline",
        "CPU calibration only accepts the development split `baseline`; the frozen holdout must remain sealed"
    );
    let labels = load_labels(labels_path, split)?;
    let goal_final = load_runs(paths.goal_final)?;
    let mandatory = load_runs(paths.mandatory)?;
    let mandatory_recovery = load_runs(paths.mandatory_recovery)?;
    let complete = load_runs(paths.complete)?;
    let nli = load_runs(paths.nli)?;
    let samples = build_samples(
        &labels,
        &goal_final,
        &mandatory,
        &mandatory_recovery,
        &complete,
        &nli,
    )?;
    let source_sha256 = BTreeMap::from([
        ("labels".into(), hash_file(labels_path)?),
        ("goal_final".into(), hash_file(paths.goal_final)?),
        ("mandatory".into(), hash_file(paths.mandatory)?),
        (
            "mandatory_recovery".into(),
            hash_file(paths.mandatory_recovery)?,
        ),
        ("complete".into(), hash_file(paths.complete)?),
        ("nli".into(), hash_file(paths.nli)?),
    ]);
    calibrate_samples(
        split,
        samples,
        FEATURE_SET_VERSION,
        feature_names(),
        source_sha256,
    )
}

pub fn calibrate_single(
    labels_path: &Path,
    split: &str,
    results_path: &Path,
) -> Result<CalibrationReport> {
    anyhow::ensure!(
        split == "baseline",
        "CPU calibration only accepts the development split `baseline`; the frozen holdout must remain sealed"
    );
    let labels = load_labels(labels_path, split)?;
    let results = load_runs(results_path)?;
    let expected = labels
        .iter()
        .map(|label| label.trace_id.clone())
        .collect::<BTreeSet<_>>();
    let actual = results.keys().cloned().collect::<BTreeSet<_>>();
    anyhow::ensure!(
        actual == expected,
        "single-feature target set differs from selected labels (missing {}, extra {})",
        expected.difference(&actual).count(),
        actual.difference(&expected).count()
    );
    let feature = single_feature_identity(results.values())?;
    let samples = labels
        .iter()
        .map(|label| {
            let result = &results[&label.trace_id];
            anyhow::ensure!(
                result.mandatory_facts_omitted == 0,
                "single-feature projection omitted mandatory facts for {}",
                label.trace_id
            );
            Ok(Sample {
                group_key: label.group_key.clone(),
                source: label.slice.source.clone(),
                domain_family: label.slice.domain_family.clone(),
                incomplete: !label.resolved,
                // Binary judges store completed minus incomplete. Negating it gives
                // the calibrator an interpretable incomplete/failure score.
                features: vec![-raw_logit(result, feature.source_name)?],
            })
        })
        .collect::<Result<Vec<_>>>()?;
    calibrate_samples(
        split,
        samples,
        feature.feature_set_version,
        vec![feature.feature_name.into()],
        BTreeMap::from([
            ("labels".into(), hash_file(labels_path)?),
            (feature.source_name.into(), hash_file(results_path)?),
        ]),
    )
}

pub fn calibrate_structured(
    labels_path: &Path,
    split: &str,
    features_path: &Path,
) -> Result<CalibrationReport> {
    anyhow::ensure!(
        split == "baseline",
        "CPU calibration only accepts the development split `baseline`; the frozen holdout must remain sealed"
    );
    let labels = load_labels(labels_path, split)?;
    let features = load_feature_records(features_path)?;
    let expected = labels
        .iter()
        .map(|label| label.trace_id.clone())
        .collect::<BTreeSet<_>>();
    let actual = features.keys().cloned().collect::<BTreeSet<_>>();
    anyhow::ensure!(
        actual == expected,
        "structured feature target set differs from selected labels (missing {}, extra {})",
        expected.difference(&actual).count(),
        actual.difference(&expected).count()
    );
    let samples = labels
        .iter()
        .map(|label| {
            let record = &features[&label.trace_id];
            Ok(Sample {
                group_key: label.group_key.clone(),
                source: label.slice.source.clone(),
                domain_family: label.slice.domain_family.clone(),
                incomplete: !label.resolved,
                features: record.feature_values.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    calibrate_samples(
        split,
        samples,
        STRUCTURED_FEATURE_SET_VERSION,
        STRUCTURED_FEATURE_NAMES.map(String::from).to_vec(),
        BTreeMap::from([
            ("labels".into(), hash_file(labels_path)?),
            ("structured_features".into(), hash_file(features_path)?),
        ]),
    )
}

pub fn calibrate_semantic(
    labels_path: &Path,
    split: &str,
    semantic_features_path: &Path,
    structured_features_path: Option<&Path>,
) -> Result<CalibrationReport> {
    anyhow::ensure!(
        split == "baseline",
        "CPU calibration only accepts the development split `baseline`; the frozen holdout must remain sealed"
    );
    let labels = load_labels(labels_path, split)?;
    let semantic = load_semantic_feature_records(semantic_features_path)?;
    let structured = structured_features_path
        .map(load_feature_records)
        .transpose()?;
    let expected = labels
        .iter()
        .map(|label| label.trace_id.clone())
        .collect::<BTreeSet<_>>();
    let semantic_keys = semantic.keys().cloned().collect::<BTreeSet<_>>();
    anyhow::ensure!(
        semantic_keys == expected,
        "semantic feature target set differs from selected labels (missing {}, extra {})",
        expected.difference(&semantic_keys).count(),
        semantic_keys.difference(&expected).count()
    );
    if let Some(structured) = &structured {
        let structured_keys = structured.keys().cloned().collect::<BTreeSet<_>>();
        anyhow::ensure!(
            structured_keys == expected,
            "structured feature target set differs from selected labels (missing {}, extra {})",
            expected.difference(&structured_keys).count(),
            structured_keys.difference(&expected).count()
        );
    }
    let samples = labels
        .iter()
        .map(|label| {
            let semantic = &semantic[&label.trace_id];
            let mut values = structured.as_ref().map_or_else(Vec::new, |records| {
                records[&label.trace_id].feature_values.clone()
            });
            values.extend_from_slice(&semantic.feature_values);
            Ok(Sample {
                group_key: label.group_key.clone(),
                source: label.slice.source.clone(),
                domain_family: label.slice.domain_family.clone(),
                incomplete: !label.resolved,
                features: values,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let (feature_set_version, mut feature_names) = if structured.is_some() {
        (
            FUSED_FEATURE_SET_VERSION,
            STRUCTURED_FEATURE_NAMES.map(String::from).to_vec(),
        )
    } else {
        (SEMANTIC_FEATURE_SET_VERSION, Vec::new())
    };
    feature_names.extend(SEMANTIC_FEATURE_NAMES.map(String::from));
    let mut source_sha256 = BTreeMap::from([
        ("labels".into(), hash_file(labels_path)?),
        (
            "semantic_features".into(),
            hash_file(semantic_features_path)?,
        ),
    ]);
    if let Some(path) = structured_features_path {
        source_sha256.insert("structured_features".into(), hash_file(path)?);
    }
    calibrate_samples(
        split,
        samples,
        feature_set_version,
        feature_names,
        source_sha256,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SingleFeatureIdentity {
    feature_set_version: &'static str,
    feature_name: &'static str,
    source_name: &'static str,
}

fn single_feature_identity<'a>(
    results: impl Iterator<Item = &'a RunRecord>,
) -> Result<SingleFeatureIdentity> {
    let identities = results
        .map(|result| {
            (
                result.decision.inference.model_id.as_str(),
                result.decision.inference.prompt_version.as_deref(),
            )
        })
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        identities.len() == 1,
        "single-feature calibration requires one model and prompt version, found {}",
        identities.len()
    );
    let (model_id, prompt_version) = identities
        .into_iter()
        .next()
        .context("single-feature calibration requires at least one result")?;
    match prompt_version {
        Some(CLOUD_PROMPT_VERSION) => Ok(SingleFeatureIdentity {
            feature_set_version: CLOUD_SINGLE_FEATURE_SET_VERSION,
            feature_name: "cloud_compact_incomplete_logit",
            source_name: "compact_cloud_score",
        }),
        Some(SMOLLM_PROMPT_VERSION) => Ok(SingleFeatureIdentity {
            feature_set_version: SMOLLM_SINGLE_FEATURE_SET_VERSION,
            feature_name: "smollm_mandatory_recovery_incomplete_logit",
            source_name: "mandatory_recovery",
        }),
        prompt_version => anyhow::bail!(
            "unsupported single-feature inference identity: model `{model_id}`, prompt version `{}`",
            prompt_version.unwrap_or("missing")
        ),
    }
}

fn calibrate_samples(
    split: &str,
    samples: Vec<Sample>,
    feature_set_version: &str,
    feature_names: Vec<String>,
    source_sha256: BTreeMap<String, String>,
) -> Result<CalibrationReport> {
    anyhow::ensure!(
        !samples.is_empty(),
        "cannot calibrate an empty development set"
    );

    let reports = std::thread::scope(|scope| -> Result<Vec<SeedReport>> {
        let handles = SEEDS
            .iter()
            .map(|seed| scope.spawn(|| run_nested_oof(&samples, *seed)))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("nested calibration worker panicked"))?
            })
            .collect()
    })?;
    let leave_one_source_out = evaluate_held_out_cohorts(&samples, CohortDimension::Source)?;
    let leave_one_domain_out = evaluate_held_out_cohorts(&samples, CohortDimension::Domain)?;
    let stability = stability_report(&reports, &leave_one_source_out, &leave_one_domain_out);

    let all_indices = (0..samples.len()).collect::<Vec<_>>();
    let final_hyperparameters =
        if distinct_cohort_count(&samples, &all_indices, CohortDimension::Source) >= 3 {
            select_hyperparameters_by_cohort(&samples, &all_indices, CohortDimension::Source)?
        } else {
            select_hyperparameters(&samples, &all_indices, OUTER_FOLDS, SEEDS[0] ^ 0xA11C_E5E5)?
        };
    let standardizer = Standardizer::fit(&samples, &all_indices)?;
    let model = LogisticModel::fit(
        &samples,
        &all_indices,
        &standardizer,
        final_hyperparameters.alpha,
    )?;
    let model_id = canonical_content_id(
        MODEL_SCHEMA_VERSION,
        &serde_json::json!({
            "feature_set_version": feature_set_version,
            "feature_names": feature_names,
            "fitted_on_split": split,
            "fitted_samples": samples.len(),
            "alpha": final_hyperparameters.alpha,
            "threshold": final_hyperparameters.threshold,
            "means": standardizer.means,
            "scales": standardizer.scales,
            "weights": model.weights,
            "intercept": model.intercept,
        }),
    )?;
    let final_model = LogisticArtifact {
        schema_version: MODEL_SCHEMA_VERSION.into(),
        model_id,
        feature_set_version: feature_set_version.into(),
        feature_names: feature_names.clone(),
        fitted_on_split: split.into(),
        fitted_samples: samples.len(),
        alpha: final_hyperparameters.alpha,
        threshold: final_hyperparameters.threshold,
        means: standardizer.means,
        scales: standardizer.scales,
        weights: model.weights,
        intercept: model.intercept,
    };

    let groups = samples
        .iter()
        .map(|sample| sample.group_key.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let incomplete_count = samples.iter().filter(|sample| sample.incomplete).count();
    Ok(CalibrationReport {
        schema_version: REPORT_SCHEMA_VERSION.into(),
        feature_set_version: feature_set_version.into(),
        training_split: split.into(),
        holdout_evaluated: false,
        sample_count: samples.len(),
        group_count: groups,
        incomplete_count,
        completed_count: samples.len() - incomplete_count,
        feature_names,
        alpha_candidates: ALPHAS.to_vec(),
        seeds: SEEDS.to_vec(),
        source_sha256,
        nested_out_of_fold: reports,
        leave_one_source_out,
        leave_one_domain_out,
        stability,
        final_model,
    })
}

pub fn write_report(report: &CalibrationReport, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, report)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn load_labels(path: &Path, split: &str) -> Result<Vec<ResolutionLabel>> {
    let mut labels = Vec::new();
    for (line_number, line) in lines(path)?.enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let label: ResolutionLabel = serde_json::from_str(&line)
            .with_context(|| format!("invalid label at {}:{}", path.display(), line_number + 1))?;
        if label.split == split {
            labels.push(label);
        }
    }
    anyhow::ensure!(!labels.is_empty(), "no labels found for split {split:?}");
    let unique = labels
        .iter()
        .map(|label| label.trace_id.as_str())
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        unique.len() == labels.len(),
        "duplicate trace IDs in labels"
    );
    Ok(labels)
}

fn load_runs(path: &Path) -> Result<BTreeMap<String, RunRecord>> {
    let mut runs = BTreeMap::new();
    for (line_number, line) in lines(path)?.enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record: RunRecord = serde_json::from_str(&line)
            .with_context(|| format!("invalid result at {}:{}", path.display(), line_number + 1))?;
        anyhow::ensure!(
            record.target_key == record.decision.target_key,
            "outer and decision target keys disagree in {}",
            path.display()
        );
        let key = record.target_key.clone();
        anyhow::ensure!(
            runs.insert(key.clone(), record).is_none(),
            "duplicate {key}"
        );
    }
    anyhow::ensure!(!runs.is_empty(), "{} contains no results", path.display());
    Ok(runs)
}

fn lines(path: &Path) -> Result<impl Iterator<Item = std::io::Result<String>>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    Ok(BufReader::new(file).lines())
}

fn build_samples(
    labels: &[ResolutionLabel],
    goal_final: &BTreeMap<String, RunRecord>,
    mandatory: &BTreeMap<String, RunRecord>,
    mandatory_recovery: &BTreeMap<String, RunRecord>,
    complete: &BTreeMap<String, RunRecord>,
    nli: &BTreeMap<String, RunRecord>,
) -> Result<Vec<Sample>> {
    let expected = labels
        .iter()
        .map(|label| label.trace_id.clone())
        .collect::<BTreeSet<_>>();
    for (name, records) in [
        ("goal_final", goal_final),
        ("mandatory", mandatory),
        ("mandatory_recovery", mandatory_recovery),
        ("complete", complete),
        ("nli", nli),
    ] {
        let actual = records.keys().cloned().collect::<BTreeSet<_>>();
        anyhow::ensure!(
            actual == expected,
            "{name} target set differs from selected labels (missing {}, extra {})",
            expected.difference(&actual).count(),
            actual.difference(&expected).count()
        );
    }

    let mut samples = Vec::with_capacity(labels.len());
    for label in labels {
        let key = &label.trace_id;
        let variants = [
            &goal_final[key],
            &mandatory[key],
            &mandatory_recovery[key],
            &complete[key],
            &nli[key],
        ];
        let revision = &variants[0].decision.target_revision;
        let binding = &variants[0].decision.trace_context_binding_id;
        anyhow::ensure!(
            variants
                .iter()
                .all(|run| &run.decision.target_revision == revision),
            "target revision mismatch for {key}"
        );
        anyhow::ensure!(
            variants
                .iter()
                .all(|run| &run.decision.trace_context_binding_id == binding),
            "trace context binding mismatch for {key}"
        );
        anyhow::ensure!(
            mandatory_recovery[key].mandatory_facts_omitted == 0
                && complete[key].mandatory_facts_omitted == 0,
            "production-candidate projections omitted mandatory facts for {key}"
        );
        let diagnostics = nli[key]
            .nli_diagnostics
            .as_ref()
            .with_context(|| format!("NLI diagnostics missing for {key}"))?;
        let decisive = diagnostics
            .logits
            .entailment
            .max(diagnostics.logits.contradiction);
        let features = vec![
            raw_logit(&goal_final[key], "goal_final")?,
            raw_logit(&mandatory[key], "mandatory")?,
            raw_logit(&mandatory_recovery[key], "mandatory_recovery")?,
            raw_logit(&complete[key], "complete")?,
            diagnostics.logits.entailment - diagnostics.logits.contradiction,
            diagnostics.logits.neutral - decisive,
        ];
        anyhow::ensure!(
            features.iter().all(|value| value.is_finite()),
            "non-finite feature for {key}"
        );
        samples.push(Sample {
            group_key: label.group_key.clone(),
            source: label.slice.source.clone(),
            domain_family: label.slice.domain_family.clone(),
            incomplete: !label.resolved,
            features,
        });
    }
    Ok(samples)
}

fn raw_logit(record: &RunRecord, source: &str) -> Result<f64> {
    let value = record.decision.raw_logit_difference.with_context(|| {
        format!(
            "{source} lacks a decisive raw logit for {}",
            record.target_key
        )
    })?;
    anyhow::ensure!(value.is_finite(), "{source} logit is non-finite");
    Ok(value)
}

#[derive(Debug, Clone, Copy)]
enum CohortDimension {
    Source,
    Domain,
}

fn evaluate_held_out_cohorts(
    samples: &[Sample],
    dimension: CohortDimension,
) -> Result<Vec<CohortReport>> {
    let cohorts = samples
        .iter()
        .map(|sample| cohort_value(sample, dimension))
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();
    if cohorts.len() < 2 {
        return Ok(Vec::new());
    }
    std::thread::scope(|scope| -> Result<Vec<CohortReport>> {
        let handles = cohorts
            .iter()
            .map(|cohort| {
                scope.spawn(|| -> Result<CohortReport> {
                    let train = samples
                        .iter()
                        .enumerate()
                        .filter_map(|(index, sample)| {
                            (cohort_value(sample, dimension) != *cohort).then_some(index)
                        })
                        .collect::<Vec<_>>();
                    let validation = samples
                        .iter()
                        .enumerate()
                        .filter_map(|(index, sample)| {
                            (cohort_value(sample, dimension) == *cohort).then_some(index)
                        })
                        .collect::<Vec<_>>();
                    anyhow::ensure!(
                        !train.is_empty() && !validation.is_empty(),
                        "empty cohort split"
                    );
                    let hyperparameters = if distinct_cohort_count(samples, &train, dimension) >= 3
                    {
                        select_hyperparameters_by_cohort(samples, &train, dimension)?
                    } else {
                        select_hyperparameters(
                            samples,
                            &train,
                            INNER_FOLDS,
                            SEEDS[0] ^ stable_hash(0xC0A0_2700, cohort),
                        )?
                    };
                    let standardizer = Standardizer::fit(samples, &train)?;
                    let model =
                        LogisticModel::fit(samples, &train, &standardizer, hyperparameters.alpha)?;
                    let probabilities = validation
                        .iter()
                        .map(|index| {
                            model.predict(&standardizer.transform(&samples[*index].features))
                        })
                        .collect::<Vec<_>>();
                    let labels = validation
                        .iter()
                        .map(|index| samples[*index].incomplete)
                        .collect::<Vec<_>>();
                    let predictions = probabilities
                        .iter()
                        .map(|probability| *probability >= hyperparameters.threshold)
                        .collect::<Vec<_>>();
                    Ok(CohortReport {
                        held_out: (*cohort).into(),
                        training_samples: train.len(),
                        validation_samples: validation.len(),
                        validation_groups: validation
                            .iter()
                            .map(|index| samples[*index].group_key.as_str())
                            .collect::<BTreeSet<_>>()
                            .len(),
                        alpha: hyperparameters.alpha,
                        threshold: hyperparameters.threshold,
                        metrics: score(&labels, &predictions, &probabilities),
                    })
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("held-out cohort worker panicked"))?
            })
            .collect()
    })
}

fn cohort_value(sample: &Sample, dimension: CohortDimension) -> &str {
    match dimension {
        CohortDimension::Source => &sample.source,
        CohortDimension::Domain => &sample.domain_family,
    }
}

fn distinct_cohort_count(
    samples: &[Sample],
    indices: &[usize],
    dimension: CohortDimension,
) -> usize {
    indices
        .iter()
        .map(|index| cohort_value(&samples[*index], dimension))
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>()
        .len()
}

fn select_hyperparameters_by_cohort(
    samples: &[Sample],
    indices: &[usize],
    dimension: CohortDimension,
) -> Result<Hyperparameters> {
    let cohorts = indices
        .iter()
        .map(|index| cohort_value(&samples[*index], dimension))
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(cohorts.len() >= 3, "fewer than three calibration cohorts");
    let mut best: Option<(Metrics, Hyperparameters)> = None;
    for alpha in ALPHAS {
        let mut probabilities = Vec::with_capacity(indices.len());
        let mut labels = Vec::with_capacity(indices.len());
        for cohort in &cohorts {
            let train = indices
                .iter()
                .copied()
                .filter(|index| cohort_value(&samples[*index], dimension) != *cohort)
                .collect::<Vec<_>>();
            let validation = indices
                .iter()
                .copied()
                .filter(|index| cohort_value(&samples[*index], dimension) == *cohort)
                .collect::<Vec<_>>();
            anyhow::ensure!(
                !train.is_empty() && !validation.is_empty(),
                "empty cohort fold"
            );
            let standardizer = Standardizer::fit(samples, &train)?;
            let model = LogisticModel::fit(samples, &train, &standardizer, alpha)?;
            for index in validation {
                probabilities
                    .push(model.predict(&standardizer.transform(&samples[index].features)));
                labels.push(samples[index].incomplete);
            }
        }
        let (threshold, metrics) = best_threshold(&labels, &probabilities);
        let candidate = Hyperparameters { alpha, threshold };
        if best
            .as_ref()
            .is_none_or(|(current, current_hp)| better(metrics, candidate, *current, *current_hp))
        {
            best = Some((metrics, candidate));
        }
    }
    best.map(|(_, hyperparameters)| hyperparameters)
        .context("no cohort hyperparameters evaluated")
}

fn run_nested_oof(samples: &[Sample], seed: u64) -> Result<SeedReport> {
    let all_indices = (0..samples.len()).collect::<Vec<_>>();
    let assignments = grouped_stratified_folds(samples, &all_indices, OUTER_FOLDS, seed)?;
    let mut probabilities = vec![f64::NAN; samples.len()];
    let mut predictions = vec![false; samples.len()];
    let mut fold_reports = Vec::with_capacity(OUTER_FOLDS);
    for fold in 0..OUTER_FOLDS {
        let train = all_indices
            .iter()
            .copied()
            .filter(|index| assignments[*index] != fold)
            .collect::<Vec<_>>();
        let validation = all_indices
            .iter()
            .copied()
            .filter(|index| assignments[*index] == fold)
            .collect::<Vec<_>>();
        anyhow::ensure!(
            !train.is_empty() && !validation.is_empty(),
            "empty outer fold"
        );
        let hyperparameters = select_hyperparameters(
            samples,
            &train,
            INNER_FOLDS,
            seed ^ ((fold as u64 + 1) * 0x9E37_79B9),
        )?;
        let standardizer = Standardizer::fit(samples, &train)?;
        let model = LogisticModel::fit(samples, &train, &standardizer, hyperparameters.alpha)?;
        for index in &validation {
            let probability = model.predict(&standardizer.transform(&samples[*index].features));
            probabilities[*index] = probability;
            predictions[*index] = probability >= hyperparameters.threshold;
        }
        fold_reports.push(FoldReport {
            fold,
            training_samples: train.len(),
            validation_samples: validation.len(),
            alpha: hyperparameters.alpha,
            threshold: hyperparameters.threshold,
        });
    }
    anyhow::ensure!(
        probabilities.iter().all(|value| value.is_finite()),
        "not every sample received an out-of-fold prediction"
    );
    let labels = samples
        .iter()
        .map(|sample| sample.incomplete)
        .collect::<Vec<_>>();
    Ok(SeedReport {
        seed,
        metrics: score(&labels, &predictions, &probabilities),
        folds: fold_reports,
    })
}

fn select_hyperparameters(
    samples: &[Sample],
    indices: &[usize],
    folds: usize,
    seed: u64,
) -> Result<Hyperparameters> {
    let assignments = grouped_stratified_folds(samples, indices, folds, seed)?;
    let mut best: Option<(Metrics, Hyperparameters)> = None;
    for alpha in ALPHAS {
        let mut probabilities = Vec::with_capacity(indices.len());
        let mut labels = Vec::with_capacity(indices.len());
        for fold in 0..folds {
            let train = indices
                .iter()
                .copied()
                .filter(|index| assignments[*index] != fold)
                .collect::<Vec<_>>();
            let validation = indices
                .iter()
                .copied()
                .filter(|index| assignments[*index] == fold)
                .collect::<Vec<_>>();
            anyhow::ensure!(
                !train.is_empty() && !validation.is_empty(),
                "empty inner fold"
            );
            let standardizer = Standardizer::fit(samples, &train)?;
            let model = LogisticModel::fit(samples, &train, &standardizer, alpha)?;
            for index in validation {
                probabilities
                    .push(model.predict(&standardizer.transform(&samples[index].features)));
                labels.push(samples[index].incomplete);
            }
        }
        let (threshold, metrics) = best_threshold(&labels, &probabilities);
        let candidate = Hyperparameters { alpha, threshold };
        if best
            .as_ref()
            .is_none_or(|(current, current_hp)| better(metrics, candidate, *current, *current_hp))
        {
            best = Some((metrics, candidate));
        }
    }
    best.map(|(_, hyperparameters)| hyperparameters)
        .context("no hyperparameters evaluated")
}

fn better(
    candidate: Metrics,
    candidate_hp: Hyperparameters,
    current: Metrics,
    current_hp: Hyperparameters,
) -> bool {
    compare_f64(candidate.mcc, current.mcc)
        .then_with(|| compare_f64(candidate.f1, current.f1))
        .then_with(|| compare_f64(candidate.accuracy, current.accuracy))
        .then_with(|| compare_f64(-candidate_hp.alpha, -current_hp.alpha))
        .then_with(|| {
            compare_f64(
                -(candidate_hp.threshold - 0.5).abs(),
                -(current_hp.threshold - 0.5).abs(),
            )
        })
        == Ordering::Greater
}

fn grouped_stratified_folds(
    samples: &[Sample],
    indices: &[usize],
    folds: usize,
    seed: u64,
) -> Result<Vec<usize>> {
    anyhow::ensure!(folds >= 2, "at least two folds are required");
    let mut groups = BTreeMap::<&str, Vec<usize>>::new();
    for index in indices {
        let sample = &samples[*index];
        groups.entry(&sample.group_key).or_default().push(*index);
    }
    anyhow::ensure!(groups.len() >= folds, "fewer groups than folds");
    let total_positive = indices
        .iter()
        .filter(|index| samples[**index].incomplete)
        .count() as f64;
    let total_negative = indices.len() as f64 - total_positive;
    let target_positive = total_positive / folds as f64;
    let target_negative = total_negative / folds as f64;
    let target_size = indices.len() as f64 / folds as f64;
    let mut ordered_groups = groups.into_iter().collect::<Vec<_>>();
    ordered_groups.sort_by(|(left_key, left), (right_key, right)| {
        right
            .len()
            .cmp(&left.len())
            .then_with(|| stable_hash(seed, left_key).cmp(&stable_hash(seed, right_key)))
    });

    let mut assignments = vec![usize::MAX; samples.len()];
    let mut fold_positive = vec![0usize; folds];
    let mut fold_negative = vec![0usize; folds];
    for (group_index, (_, members)) in ordered_groups.into_iter().enumerate() {
        let positive = members
            .iter()
            .filter(|index| samples[**index].incomplete)
            .count();
        let negative = members.len() - positive;
        let fold = if group_index < folds {
            group_index
        } else {
            (0..folds)
                .min_by(|left, right| {
                    let score = |candidate: usize| {
                        (0..folds)
                            .map(|fold| {
                                let next_positive = fold_positive[fold]
                                    + if fold == candidate { positive } else { 0 };
                                let next_negative = fold_negative[fold]
                                    + if fold == candidate { negative } else { 0 };
                                let next_size = next_positive + next_negative;
                                ((next_positive as f64 - target_positive)
                                    / target_positive.max(1.0))
                                .powi(2)
                                    + ((next_negative as f64 - target_negative)
                                        / target_negative.max(1.0))
                                    .powi(2)
                                    + 0.25
                                        * ((next_size as f64 - target_size) / target_size.max(1.0))
                                            .powi(2)
                            })
                            .sum::<f64>()
                    };
                    score(*left)
                        .total_cmp(&score(*right))
                        .then_with(|| {
                            (fold_positive[*left] + fold_negative[*left])
                                .cmp(&(fold_positive[*right] + fold_negative[*right]))
                        })
                        .then_with(|| left.cmp(right))
                })
                .expect("fold list is non-empty")
        };
        for index in members {
            assignments[index] = fold;
        }
        fold_positive[fold] += positive;
        fold_negative[fold] += negative;
    }
    anyhow::ensure!(
        indices
            .iter()
            .all(|index| assignments[*index] != usize::MAX),
        "fold assignment is incomplete"
    );
    Ok(assignments)
}

fn stable_hash(seed: u64, value: &str) -> u64 {
    let mut digest = Sha256::new();
    digest.update(seed.to_le_bytes());
    digest.update(value.as_bytes());
    let bytes = digest.finalize();
    u64::from_le_bytes(bytes[..8].try_into().expect("SHA-256 has eight bytes"))
}

impl Standardizer {
    fn fit(samples: &[Sample], indices: &[usize]) -> Result<Self> {
        anyhow::ensure!(
            !indices.is_empty(),
            "cannot standardize an empty training set"
        );
        let dimensions = samples[indices[0]].features.len();
        let mut means = vec![0.0; dimensions];
        for index in indices {
            anyhow::ensure!(
                samples[*index].features.len() == dimensions,
                "feature dimension mismatch"
            );
            for (mean, value) in means.iter_mut().zip(&samples[*index].features) {
                *mean += value;
            }
        }
        for mean in &mut means {
            *mean /= indices.len() as f64;
        }
        let mut scales = vec![0.0; dimensions];
        for index in indices {
            for ((scale, value), mean) in
                scales.iter_mut().zip(&samples[*index].features).zip(&means)
            {
                *scale += (value - mean).powi(2);
            }
        }
        for scale in &mut scales {
            *scale = (*scale / indices.len() as f64).sqrt();
            if *scale < 1e-9 {
                *scale = 1.0;
            }
        }
        Ok(Self { means, scales })
    }

    fn transform(&self, features: &[f64]) -> Vec<f64> {
        features
            .iter()
            .zip(&self.means)
            .zip(&self.scales)
            .map(|((value, mean), scale)| (value - mean) / scale)
            .collect()
    }
}

impl LogisticModel {
    fn fit(
        samples: &[Sample],
        indices: &[usize],
        standardizer: &Standardizer,
        alpha: f64,
    ) -> Result<Self> {
        let dimensions = standardizer.means.len();
        let positives = indices
            .iter()
            .filter(|index| samples[**index].incomplete)
            .count();
        anyhow::ensure!(
            positives > 0 && positives < indices.len(),
            "training fold lacks a class"
        );
        let prevalence = positives as f64 / indices.len() as f64;
        let mut model = Self {
            weights: vec![0.0; dimensions],
            intercept: (prevalence / (1.0 - prevalence)).ln(),
        };
        let transformed = indices
            .iter()
            .map(|index| standardizer.transform(&samples[*index].features))
            .collect::<Vec<_>>();
        let labels = indices
            .iter()
            .map(|index| f64::from(samples[*index].incomplete))
            .collect::<Vec<_>>();
        for _ in 0..1_000 {
            let (loss, gradient, intercept_gradient) =
                model.objective_gradient(&transformed, &labels, alpha);
            let norm = gradient.iter().map(|value| value * value).sum::<f64>()
                + intercept_gradient * intercept_gradient;
            if norm.sqrt() < 1e-8 {
                break;
            }
            let mut step = 1.0;
            let mut accepted = None;
            for _ in 0..30 {
                let candidate = Self {
                    weights: model
                        .weights
                        .iter()
                        .zip(&gradient)
                        .map(|(weight, gradient)| weight - step * gradient)
                        .collect(),
                    intercept: model.intercept - step * intercept_gradient,
                };
                let candidate_loss = candidate.objective(&transformed, &labels, alpha);
                if candidate_loss <= loss - 1e-4 * step * norm {
                    accepted = Some(candidate);
                    break;
                }
                step *= 0.5;
            }
            let Some(candidate) = accepted else {
                break;
            };
            model = candidate;
        }
        anyhow::ensure!(
            model.intercept.is_finite() && model.weights.iter().all(|value| value.is_finite()),
            "logistic training produced non-finite parameters"
        );
        Ok(model)
    }

    fn objective_gradient(
        &self,
        features: &[Vec<f64>],
        labels: &[f64],
        alpha: f64,
    ) -> (f64, Vec<f64>, f64) {
        let mut gradient = vec![0.0; self.weights.len()];
        let mut intercept_gradient = 0.0;
        for (features, label) in features.iter().zip(labels) {
            let error = self.predict(features) - label;
            for (gradient, feature) in gradient.iter_mut().zip(features) {
                *gradient += error * feature;
            }
            intercept_gradient += error;
        }
        for (gradient, weight) in gradient.iter_mut().zip(&self.weights) {
            *gradient = *gradient / labels.len() as f64 + alpha * weight;
        }
        intercept_gradient /= labels.len() as f64;
        (
            self.objective(features, labels, alpha),
            gradient,
            intercept_gradient,
        )
    }

    fn objective(&self, features: &[Vec<f64>], labels: &[f64], alpha: f64) -> f64 {
        let data_loss = features
            .iter()
            .zip(labels)
            .map(|(features, label)| {
                let logit = self.logit(features);
                logit.max(0.0) - label * logit + (-logit.abs()).exp().ln_1p()
            })
            .sum::<f64>()
            / labels.len() as f64;
        data_loss + 0.5 * alpha * self.weights.iter().map(|value| value * value).sum::<f64>()
    }

    fn logit(&self, features: &[f64]) -> f64 {
        self.intercept
            + self
                .weights
                .iter()
                .zip(features)
                .map(|(weight, feature)| weight * feature)
                .sum::<f64>()
    }

    fn predict(&self, features: &[f64]) -> f64 {
        sigmoid(self.logit(features))
    }
}

fn best_threshold(labels: &[bool], probabilities: &[f64]) -> (f64, Metrics) {
    let mut sorted = probabilities.to_vec();
    sorted.sort_by(|left, right| left.total_cmp(right));
    sorted.dedup_by(|left, right| (*left - *right).abs() < 1e-12);
    let mut thresholds = vec![0.0, 0.5, 1.0 + f64::EPSILON];
    thresholds.extend(sorted.windows(2).map(|pair| (pair[0] + pair[1]) / 2.0));
    thresholds.extend(sorted);
    thresholds.sort_by(|left, right| left.total_cmp(right));
    thresholds.dedup_by(|left, right| (*left - *right).abs() < 1e-12);
    let mut best: Option<(f64, Metrics)> = None;
    for threshold in thresholds {
        let predictions = probabilities
            .iter()
            .map(|probability| *probability >= threshold)
            .collect::<Vec<_>>();
        let metrics = score(labels, &predictions, probabilities);
        if best.as_ref().is_none_or(|(current_threshold, current)| {
            compare_f64(metrics.mcc, current.mcc)
                .then_with(|| compare_f64(metrics.f1, current.f1))
                .then_with(|| compare_f64(metrics.accuracy, current.accuracy))
                .then_with(|| {
                    compare_f64(-(threshold - 0.5).abs(), -(*current_threshold - 0.5).abs())
                })
                == Ordering::Greater
        }) {
            best = Some((threshold, metrics));
        }
    }
    best.expect("at least one threshold is available")
}

fn score(labels: &[bool], predictions: &[bool], probabilities: &[f64]) -> Metrics {
    let mut tp = 0u64;
    let mut fp = 0u64;
    let mut tn = 0u64;
    let mut fn_ = 0u64;
    for (label, prediction) in labels.iter().zip(predictions) {
        match (*label, *prediction) {
            (true, true) => tp += 1,
            (false, true) => fp += 1,
            (false, false) => tn += 1,
            (true, false) => fn_ += 1,
        }
    }
    let precision = ratio(tp, tp + fp);
    let recall = ratio(tp, tp + fn_);
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    let denominator =
        ((tp + fp) as f64 * (tp + fn_) as f64 * (tn + fp) as f64 * (tn + fn_) as f64).sqrt();
    let mcc = if denominator == 0.0 {
        0.0
    } else {
        (tp as f64 * tn as f64 - fp as f64 * fn_ as f64) / denominator
    };
    Metrics {
        true_positive: tp,
        false_positive: fp,
        true_negative: tn,
        false_negative: fn_,
        precision,
        recall,
        f1,
        mcc,
        accuracy: ratio(tp + tn, labels.len() as u64),
        auroc: auroc(labels, probabilities),
        brier: probabilities
            .iter()
            .zip(labels)
            .map(|(probability, label)| (probability - f64::from(*label)).powi(2))
            .sum::<f64>()
            / labels.len() as f64,
        expected_calibration_error: expected_calibration_error(labels, probabilities, 10),
    }
}

fn auroc(labels: &[bool], probabilities: &[f64]) -> f64 {
    let positives = probabilities
        .iter()
        .zip(labels)
        .filter_map(|(probability, label)| label.then_some(*probability))
        .collect::<Vec<_>>();
    let negatives = probabilities
        .iter()
        .zip(labels)
        .filter_map(|(probability, label)| (!label).then_some(*probability))
        .collect::<Vec<_>>();
    if positives.is_empty() || negatives.is_empty() {
        return 0.5;
    }
    let wins = positives
        .iter()
        .flat_map(|positive| {
            negatives.iter().map(move |negative| {
                if positive > negative {
                    1.0
                } else if positive == negative {
                    0.5
                } else {
                    0.0
                }
            })
        })
        .sum::<f64>();
    wins / (positives.len() * negatives.len()) as f64
}

fn expected_calibration_error(labels: &[bool], probabilities: &[f64], bins: usize) -> f64 {
    let mut error = 0.0;
    for bin in 0..bins {
        let lower = bin as f64 / bins as f64;
        let upper = (bin + 1) as f64 / bins as f64;
        let members = probabilities
            .iter()
            .zip(labels)
            .filter(|(probability, _)| {
                **probability >= lower && (bin + 1 == bins || **probability < upper)
            })
            .collect::<Vec<_>>();
        if members.is_empty() {
            continue;
        }
        let confidence = members
            .iter()
            .map(|(probability, _)| **probability)
            .sum::<f64>()
            / members.len() as f64;
        let frequency =
            members.iter().filter(|(_, label)| **label).count() as f64 / members.len() as f64;
        error += members.len() as f64 / labels.len() as f64 * (confidence - frequency).abs();
    }
    error
}

fn stability_report(
    reports: &[SeedReport],
    source_reports: &[CohortReport],
    domain_reports: &[CohortReport],
) -> StabilityReport {
    let mut f1 = reports
        .iter()
        .map(|report| report.metrics.f1)
        .collect::<Vec<_>>();
    let mut mcc = reports
        .iter()
        .map(|report| report.metrics.mcc)
        .collect::<Vec<_>>();
    f1.sort_by(|left, right| left.total_cmp(right));
    mcc.sort_by(|left, right| left.total_cmp(right));
    let all_seeds_pass = reports
        .iter()
        .all(|report| report.metrics.f1 > F1_EXIT && report.metrics.mcc > MCC_EXIT);
    let min_leave_source_mcc = source_reports
        .iter()
        .map(|report| report.metrics.mcc)
        .min_by(f64::total_cmp);
    let min_leave_domain_mcc = domain_reports
        .iter()
        .map(|report| report.metrics.mcc)
        .min_by(f64::total_cmp);
    let all_held_out_cohorts_pass = source_reports
        .iter()
        .chain(domain_reports)
        .all(|report| report.metrics.mcc > MCC_EXIT);
    let advances_to_frozen_holdout = all_seeds_pass && all_held_out_cohorts_pass;
    StabilityReport {
        required_f1_exclusive: F1_EXIT,
        required_mcc_exclusive: MCC_EXIT,
        min_f1: f1[0],
        median_f1: f1[f1.len() / 2],
        min_mcc: mcc[0],
        median_mcc: mcc[mcc.len() / 2],
        max_mcc: *mcc.last().expect("reports are non-empty"),
        all_seeds_pass,
        min_leave_source_mcc,
        min_leave_domain_mcc,
        all_held_out_cohorts_pass,
        advances_to_frozen_holdout,
        decision: if advances_to_frozen_holdout {
            "advance_to_frozen_holdout".into()
        } else {
            "do_not_open_frozen_holdout".into()
        },
    }
}

fn feature_names() -> Vec<String> {
    FEATURE_NAMES.iter().map(|name| (*name).into()).collect()
}

fn hash_file(path: &Path) -> Result<String> {
    sha256_file(path)
        .map(|hash| format!("sha256:{hash}"))
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exp = value.exp();
        exp / (1.0 + exp)
    }
}

fn compare_f64(left: f64, right: f64) -> Ordering {
    left.total_cmp(&right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_record(model_id: &str, prompt_version: &str) -> RunRecord {
        RunRecord {
            target_key: "trace-1".into(),
            mandatory_facts_omitted: 0,
            decision: DecisionRecord {
                target_key: "trace-1".into(),
                target_revision: "revision-1".into(),
                trace_context_binding_id: "binding-1".into(),
                raw_logit_difference: Some(0.5),
                inference: InferenceRecord {
                    model_id: model_id.into(),
                    prompt_version: Some(prompt_version.into()),
                },
            },
            nli_diagnostics: None,
        }
    }

    fn sample(_key: &str, group: &str, incomplete: bool, feature: f64) -> Sample {
        Sample {
            group_key: group.into(),
            source: "source-a".into(),
            domain_family: "domain-a".into(),
            incomplete,
            features: vec![feature, feature * 0.5],
        }
    }

    fn balanced_samples() -> Vec<Sample> {
        (0..20)
            .map(|index| {
                let incomplete = index >= 10;
                let feature = if incomplete {
                    1.0 + index as f64 / 100.0
                } else {
                    -1.0 - index as f64 / 100.0
                };
                sample(
                    &format!("trace-{index}"),
                    &format!("group-{index}"),
                    incomplete,
                    feature,
                )
            })
            .collect()
    }

    #[test]
    fn grouped_folds_keep_duplicate_tasks_together() {
        let mut samples = balanced_samples();
        samples.push(sample("duplicate", "group-0", false, -1.2));
        let indices = (0..samples.len()).collect::<Vec<_>>();
        let folds = grouped_stratified_folds(&samples, &indices, 5, 17).unwrap();
        assert_eq!(folds[0], folds[20]);
        assert!(folds.iter().all(|fold| *fold < 5));
    }

    #[test]
    fn fold_assignment_is_seeded_and_reproducible() {
        let samples = balanced_samples();
        let indices = (0..samples.len()).collect::<Vec<_>>();
        let first = grouped_stratified_folds(&samples, &indices, 5, 29).unwrap();
        let second = grouped_stratified_folds(&samples, &indices, 5, 29).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn nested_oof_separates_a_simple_learned_signal() {
        let samples = balanced_samples();
        let report = run_nested_oof(&samples, 43).unwrap();
        assert_eq!(report.metrics.f1, 1.0);
        assert_eq!(report.metrics.mcc, 1.0);
        assert_eq!(report.folds.len(), 5);
    }

    #[test]
    fn single_feature_identity_distinguishes_cloud_from_smollm() {
        let cloud = run_record("gpt-5.6-sol", CLOUD_PROMPT_VERSION);
        let identity = single_feature_identity([&cloud].into_iter()).unwrap();
        assert_eq!(
            identity.feature_set_version,
            CLOUD_SINGLE_FEATURE_SET_VERSION
        );
        assert_eq!(identity.feature_name, "cloud_compact_incomplete_logit");

        let local = run_record("smollm3-3b", SMOLLM_PROMPT_VERSION);
        let identity = single_feature_identity([&local].into_iter()).unwrap();
        assert_eq!(
            identity.feature_set_version,
            SMOLLM_SINGLE_FEATURE_SET_VERSION
        );
        assert_eq!(
            identity.feature_name,
            "smollm_mandatory_recovery_incomplete_logit"
        );
    }

    #[test]
    fn single_feature_identity_rejects_mixed_runs() {
        let cloud = run_record("gpt-5.6-sol", CLOUD_PROMPT_VERSION);
        let local = run_record("smollm3-3b", SMOLLM_PROMPT_VERSION);
        let error = single_feature_identity([&cloud, &local].into_iter()).unwrap_err();
        assert!(error.to_string().contains("one model and prompt version"));
    }

    #[test]
    fn standardizer_uses_only_requested_training_rows() {
        let samples = vec![
            sample("a", "a", false, 0.0),
            sample("b", "b", true, 2.0),
            sample("held-out", "held-out", true, 1_000.0),
        ];
        let standardizer = Standardizer::fit(&samples, &[0, 1]).unwrap();
        assert_eq!(standardizer.means, vec![1.0, 0.5]);
        assert_eq!(standardizer.scales, vec![1.0, 0.5]);
    }

    #[test]
    fn mixed_outcome_groups_stay_in_one_fold() {
        let mut samples = balanced_samples();
        samples[10].group_key = samples[0].group_key.clone();
        let indices = (0..samples.len()).collect::<Vec<_>>();
        let folds = grouped_stratified_folds(&samples, &indices, 5, 17).unwrap();
        assert_eq!(folds[0], folds[10]);
    }

    #[test]
    fn variable_sized_groups_do_not_starve_folds() {
        let mut samples = Vec::new();
        for group in 0..308 {
            let members = if group < 5 { 7 } else { 1 };
            for member in 0..members {
                let incomplete = (group + member) % 3 == 0;
                samples.push(sample(
                    &format!("trace-{group}-{member}"),
                    &format!("group-{group}"),
                    incomplete,
                    if incomplete { 1.0 } else { -1.0 },
                ));
            }
        }
        let indices = (0..samples.len()).collect::<Vec<_>>();
        let assignments = grouped_stratified_folds(&samples, &indices, 5, 17).unwrap();
        let mut sizes = [0_usize; 5];
        for fold in assignments {
            sizes[fold] += 1;
        }
        let spread = sizes.iter().max().unwrap() - sizes.iter().min().unwrap();
        assert!(spread <= 7, "fold sizes are unbalanced: {sizes:?}");
    }

    #[test]
    fn calibration_refuses_the_frozen_holdout_before_reading_inputs() {
        let missing = Path::new("does-not-exist");
        let error = calibrate(
            missing,
            "primary",
            LearnedResultPaths {
                goal_final: missing,
                mandatory: missing,
                mandatory_recovery: missing,
                complete: missing,
                nli: missing,
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("frozen holdout must remain sealed")
        );
    }

    #[test]
    fn held_out_source_evaluation_reports_every_source() {
        let mut samples = Vec::new();
        for source in 0..3 {
            for index in 0..10 {
                let incomplete = index >= 5;
                let mut row = sample(
                    &format!("trace-{source}-{index}"),
                    &format!("group-{source}-{index}"),
                    incomplete,
                    if incomplete { 1.0 } else { -1.0 },
                );
                row.source = format!("source-{source}");
                row.domain_family = format!("domain-{source}");
                samples.push(row);
            }
        }
        let reports = evaluate_held_out_cohorts(&samples, CohortDimension::Source).unwrap();
        assert_eq!(reports.len(), 3);
        assert!(reports.iter().all(|report| report.validation_samples == 10));
    }
}
