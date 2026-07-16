use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use perseval_store::{
    FailureFiltersV1, PipelineDiagnosticsV1, QueryScopeCriteriaV1, QueryScopeV1, WorkspaceStore,
    WorkspaceStoreLayout,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkspaceProfile {
    pub schema_version: &'static str,
    pub captured_at_unix_ms: u64,
    pub workspace: String,
    pub system: SystemProfile,
    pub open_ms: f64,
    pub query_ms: BTreeMap<String, TimingSummary>,
    pub counts: BTreeMap<String, u64>,
    pub instrumented_stage_ms: BTreeMap<String, f64>,
    pub pipeline: PipelineDiagnosticsV1,
    pub replay_accounting: Option<ReplayAccounting>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplayAccounting {
    pub client_wall_ms: f64,
    pub durable_acknowledgement_ms: f64,
    pub named_pre_ack_stage_ms: f64,
    pub admission_queue_and_ack_overhead_ms: f64,
    pub client_fixture_and_transport_overhead_ms: f64,
    pub explained_wall_ratio: f64,
}

#[derive(Debug, Deserialize)]
struct ReplayWallInput {
    elapsed_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SystemProfile {
    pub operating_system: String,
    pub architecture: String,
    pub rustc: String,
    pub build_profile: &'static str,
    pub debug_assertions: bool,
    pub package_version: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimingSummary {
    pub samples: u64,
    pub minimum: f64,
    pub median: f64,
    pub p95: f64,
    pub maximum: f64,
}

pub fn profile_workspace(
    workspace: &Path,
    replay_report: Option<&Path>,
) -> Result<WorkspaceProfile, Box<dyn Error>> {
    let layout = WorkspaceStoreLayout::new(workspace);
    let opened = Instant::now();
    let store = WorkspaceStore::open(&layout, "default")?;
    let open_ms = elapsed_ms(opened);

    let mut query_ms = BTreeMap::new();
    let mut counts = control_counts(&layout.control_database())?;
    let (run_timings, runs) = sample(5, || store.list_runs(0, 200))?;
    query_ms.insert("list_runs_200".into(), summarize(run_timings));
    counts.insert("runs_in_first_page".into(), runs.len() as u64);
    counts.insert(
        "spans_in_first_run".into(),
        runs.first().map_or(0, |run| run.span_count),
    );

    let project_id = runs.first().map(|run| run.project_id.clone());
    let filters = FailureFiltersV1 {
        scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
            project_id: project_id.clone(),
            ..QueryScopeCriteriaV1::default()
        }),
        ..FailureFiltersV1::default()
    };
    let (group_timings, groups) = sample(5, || store.list_failure_groups(&filters, 0, 200))?;
    query_ms.insert("list_failure_groups_200".into(), summarize(group_timings));
    counts.insert("groups_in_first_page".into(), groups.len() as u64);

    if let (Some(_), Some(group)) = (project_id.as_deref(), groups.first()) {
        let (occurrence_timings, occurrences) = sample(5, || {
            store.list_failure_occurrences_in_scope(&filters.scope, &group.group_id, 0, 100)
        })?;
        query_ms.insert(
            "list_failure_occurrences_100".into(),
            summarize(occurrence_timings),
        );
        counts.insert("occurrences_in_first_page".into(), occurrences.len() as u64);
        if let Some(occurrence) = occurrences.first() {
            let (evidence_timings, evidence) = sample(5, || {
                store.get_finding_evidence_in_scope(
                    &filters.scope,
                    &group.group_id,
                    &occurrence.finding.finding_id,
                    128,
                )
            })?;
            query_ms.insert(
                "get_finding_evidence_128".into(),
                summarize(evidence_timings),
            );
            counts.insert(
                "evidence_spans".into(),
                evidence.map_or(0, |evidence| evidence.spans.len() as u64),
            );
        }
    }

    let pipeline = store.pipeline_diagnostics()?;
    counts.insert("journal_backlog_rows".into(), pipeline.journal_backlog_rows);
    counts.insert(
        "analysis_backlog_rows".into(),
        pipeline.analysis_backlog_rows,
    );
    counts.insert(
        "feature_similarity_models_built".into(),
        pipeline.feature_similarity_models_built,
    );
    counts.insert(
        "feature_similarity_assignments_written".into(),
        pipeline.feature_similarity_assignments_written,
    );
    let instrumented_stage_ms = pipeline
        .stages
        .iter()
        .map(|stage| {
            (
                stage.stage.as_str().to_owned(),
                stage.total_duration_nano as f64 / 1_000_000.0,
            )
        })
        .collect();
    let mut warnings = Vec::new();
    if pipeline.stages.is_empty() {
        warnings.push(
            "No durable stage metrics exist in this workspace; replay through an instrumented build before treating this profile as a Gate A artifact."
                .into(),
        );
    }
    let replay_accounting = replay_report
        .map(|path| replay_accounting(path, &instrumented_stage_ms))
        .transpose()?;
    if let Some(accounting) = &replay_accounting {
        if accounting.explained_wall_ratio < 0.95 {
            warnings.push(format!(
                "Named server stages explain {:.1}% of replay wall time; Gate A requires at least 95%.",
                accounting.explained_wall_ratio * 100.0
            ));
        }
    } else {
        warnings.push(
            "Replay accounting was not requested; this profile alone cannot prove the REC-005 95% wall-time gate."
                .into(),
        );
    }
    Ok(WorkspaceProfile {
        schema_version: "perseval.benchmark_workspace_profile.v3",
        captured_at_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64,
        workspace: workspace.display().to_string(),
        system: SystemProfile {
            operating_system: command_output("uname", &["-srv"]),
            architecture: std::env::consts::ARCH.into(),
            rustc: command_output("rustc", &["--version"]),
            build_profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            debug_assertions: cfg!(debug_assertions),
            package_version: env!("CARGO_PKG_VERSION"),
        },
        open_ms,
        query_ms,
        counts,
        instrumented_stage_ms,
        pipeline,
        replay_accounting,
        warnings,
    })
}

fn replay_accounting(
    path: &Path,
    stages: &BTreeMap<String, f64>,
) -> Result<ReplayAccounting, Box<dyn Error>> {
    let replay: ReplayWallInput = serde_json::from_reader(std::fs::File::open(path)?)?;
    if !replay.elapsed_ms.is_finite() || replay.elapsed_ms <= 0.0 {
        return Err("replay report elapsed_ms must be finite and positive".into());
    }
    let acknowledgement = stages
        .get("durable_acknowledgement")
        .copied()
        .unwrap_or_default();
    let named_pre_ack = [
        "decode",
        "journal_build",
        "payload_blob_durability",
        "raw_blob_durability",
        "normalized_blob_durability",
        "journal_commit",
    ]
    .into_iter()
    .map(|stage| stages.get(stage).copied().unwrap_or_default())
    .sum::<f64>();
    let explained = acknowledgement.min(replay.elapsed_ms);
    Ok(ReplayAccounting {
        client_wall_ms: replay.elapsed_ms,
        durable_acknowledgement_ms: acknowledgement,
        named_pre_ack_stage_ms: named_pre_ack,
        admission_queue_and_ack_overhead_ms: (acknowledgement - named_pre_ack).max(0.0),
        client_fixture_and_transport_overhead_ms: (replay.elapsed_ms - acknowledgement).max(0.0),
        explained_wall_ratio: explained / replay.elapsed_ms,
    })
}

fn control_counts(path: &Path) -> Result<BTreeMap<String, u64>, Box<dyn Error>> {
    let connection = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    let queries = [
        ("journal_rows", "SELECT count(*) FROM ingest_journal"),
        (
            "unprojected_journal_rows",
            "SELECT count(*) FROM ingest_journal WHERE projected = 0",
        ),
        ("logical_traces", "SELECT count(*) FROM logical_traces"),
        (
            "ready_traces",
            "SELECT count(*) FROM logical_traces WHERE analysis_status = 'ready'",
        ),
        (
            "active_analysis_results",
            "SELECT count(*) FROM analysis_results WHERE active = 1",
        ),
        (
            "feature_similarity_models",
            "SELECT count(*) FROM semantic_cluster_models",
        ),
        (
            "feature_similarity_assignments",
            "SELECT count(*) FROM semantic_cluster_assignments",
        ),
        (
            "candidate_generation_jobs",
            "SELECT count(*) FROM candidate_generation_jobs",
        ),
    ];
    let mut counts = BTreeMap::new();
    for (name, query) in queries {
        let count = connection.query_row(query, [], |row| row.get::<_, i64>(0))?;
        counts.insert(name.into(), count.max(0) as u64);
    }
    Ok(counts)
}

fn sample<T, E>(
    count: usize,
    mut operation: impl FnMut() -> Result<T, E>,
) -> Result<(Vec<f64>, T), E> {
    let mut timings = Vec::with_capacity(count);
    let mut last = None;
    for _ in 0..count {
        let started = Instant::now();
        last = Some(operation()?);
        timings.push(elapsed_ms(started));
    }
    Ok((timings, last.expect("sample count is non-zero")))
}

fn summarize(mut samples: Vec<f64>) -> TimingSummary {
    samples.sort_by(f64::total_cmp);
    let p95_index = ((samples.len() as f64 * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(samples.len() - 1);
    TimingSummary {
        samples: samples.len() as u64,
        minimum: samples[0],
        median: samples[samples.len() / 2],
        p95: samples[p95_index],
        maximum: samples[samples.len() - 1],
    }
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_summary_uses_a_nearest_rank_p95() {
        let summary = summarize(vec![5.0, 1.0, 2.0, 3.0, 4.0]);

        assert_eq!(summary.minimum, 1.0);
        assert_eq!(summary.median, 3.0);
        assert_eq!(summary.p95, 5.0);
        assert_eq!(summary.maximum, 5.0);
    }

    #[test]
    fn replay_accounting_separates_exclusive_stages_from_acknowledgement_residual() {
        let directory = tempfile::tempdir().unwrap();
        let replay = directory.path().join("replay.json");
        std::fs::write(&replay, r#"{"elapsed_ms":100.0}"#).unwrap();
        let stages = BTreeMap::from([
            ("durable_acknowledgement".into(), 98.0),
            ("decode".into(), 10.0),
            ("journal_build".into(), 20.0),
            ("payload_blob_durability".into(), 7.0),
            ("raw_blob_durability".into(), 5.0),
            ("normalized_blob_durability".into(), 5.0),
            ("journal_commit".into(), 30.0),
        ]);

        let accounting = replay_accounting(&replay, &stages).unwrap();

        assert_eq!(accounting.named_pre_ack_stage_ms, 77.0);
        assert_eq!(accounting.admission_queue_and_ack_overhead_ms, 21.0);
        assert_eq!(accounting.client_fixture_and_transport_overhead_ms, 2.0);
        assert_eq!(accounting.explained_wall_ratio, 0.98);
    }
}
