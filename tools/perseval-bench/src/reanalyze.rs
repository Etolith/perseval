use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use perseval_service::{AnalysisStatus, PersevalConfigV1, ServiceRuntime};
use perseval_store::{WorkspaceStore, WorkspaceStoreLayout};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ReanalysisReport {
    pub schema_version: &'static str,
    pub workspace: PathBuf,
    pub traces: u64,
    pub ready_traces: u64,
    pub elapsed_ms: u64,
    pub feature_similarity_enabled: bool,
    pub active_detector_versions: BTreeMap<String, BTreeSet<String>>,
}

/// Opens an existing durable workspace under the current analysis definition,
/// waits for all finalized revisions to become ready, and never replays OTLP.
pub async fn reanalyze_workspace(
    workspace: &Path,
    timeout: Duration,
) -> Result<ReanalysisReport, Box<dyn Error>> {
    if !workspace.is_dir() {
        return Err(format!("workspace does not exist: {}", workspace.display()).into());
    }
    let mut config = PersevalConfigV1 {
        workspace_id: "default".into(),
        workspace_dir: workspace.to_path_buf(),
        ..PersevalConfigV1::default()
    };
    config.otlp.enabled = false;
    // Detector qualification must not rebuild the optional full-corpus cohort
    // model once per trace. Cohorts are requalified independently after their
    // scheduler is debounced.
    config.analysis.feature_similarity_enabled = false;

    let runtime = ServiceRuntime::start_embedded(config.clone())?;
    let result = wait_until_ready(&runtime, timeout).await;
    runtime.shutdown();
    drop(runtime);
    let (traces, ready_traces, elapsed_ms) = result?;
    let store = WorkspaceStore::open(&WorkspaceStoreLayout::new(workspace), &config.workspace_id)?;
    Ok(ReanalysisReport {
        schema_version: "perseval.benchmark_reanalysis.v1",
        workspace: workspace.to_path_buf(),
        traces,
        ready_traces,
        elapsed_ms,
        feature_similarity_enabled: config.analysis.feature_similarity_enabled,
        active_detector_versions: store.active_detector_versions()?,
    })
}

async fn wait_until_ready(
    runtime: &ServiceRuntime,
    timeout: Duration,
) -> Result<(u64, u64, u64), Box<dyn Error>> {
    let live = runtime
        .live()
        .ok_or("embedded reanalysis runtime stopped unexpectedly")?;
    let traces = live.run_count()?;
    let started = Instant::now();
    loop {
        let health = live.source_health()?;
        let mut ready_traces = 0_u64;
        let mut offset = 0_u64;
        while offset < traces {
            let page = live.list_runs(offset, 200)?;
            if page.is_empty() {
                break;
            }
            ready_traces = ready_traces.saturating_add(
                page.iter()
                    .filter(|run| run.analysis_status == AnalysisStatus::Ready)
                    .count() as u64,
            );
            offset = offset.saturating_add(page.len() as u64);
        }
        if ready_traces == traces
            && health.journal_lag == 0
            && health.projection_lag == 0
            && health.analysis_pending == 0
            && health.analysis_running == 0
        {
            return Ok((traces, ready_traces, started.elapsed().as_millis() as u64));
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "reanalysis timed out after {}s: ready={ready_traces}/{traces}, journal_lag={}, projection_lag={}, analysis_pending={}, analysis_running={}",
                timeout.as_secs(),
                health.journal_lag,
                health.projection_lag,
                health.analysis_pending,
                health.analysis_running
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
