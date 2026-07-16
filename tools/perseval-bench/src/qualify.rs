use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use perseval_service::{PersevalConfigV1, ServiceRuntime};
use perseval_store::AnalysisStatus;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::fetch::sha256_file;
use crate::fixture::{FixtureBuildReport, build_fixture};
use crate::isolation::{IsolationAuditReport, audit_workspace};
use crate::profile::{WorkspaceProfile, profile_workspace};
use crate::replay::{ReplayOptions, ReplayReport, replay};
use crate::score::{ScoreReport, score_workspace, write_json_report};

const QUALIFICATION_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const MAX_REPLAY_MS: f64 = 10_000.0;
const MAX_ACKNOWLEDGEMENT_P95_MS: f64 = 100.0;
const MAX_PROJECTION_CATCHUP_MS: f64 = 10_000.0;
const MAX_ANALYSIS_AFTER_PROJECTION_MS: f64 = 15_000.0;
const MAX_PEAK_RSS_BYTES: u64 = 1_500 * 1_024 * 1_024;
const MAX_COHORT_MODELS_PER_BURST: u64 = 2;

#[derive(Debug, Serialize)]
pub struct QualificationReport {
    pub schema_version: &'static str,
    pub tier: String,
    pub fixture: FixtureBuildReport,
    pub determinism: FixtureDeterminismReport,
    pub replay: ReplayReport,
    pub catchup: CatchupReport,
    pub isolation: IsolationAuditReport,
    pub score: ScoreReport,
    pub profile: WorkspaceProfile,
    pub peak_rss_bytes: u64,
    pub runtime_config: PersevalConfigV1,
    pub code: Vec<CodeIdentity>,
    pub workspace: PathBuf,
    pub replay_report: PathBuf,
    pub score_report: PathBuf,
    pub profile_report: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CatchupReport {
    pub schema_version: &'static str,
    pub projection_catchup_ms: f64,
    pub analysis_ready_ms: f64,
    pub analysis_after_projection_ms: f64,
    pub topology_ready_ms: f64,
    pub fully_ready_ms: f64,
}

#[derive(Debug, Serialize)]
pub struct FixtureDeterminismReport {
    pub schema_version: &'static str,
    pub repeated_builds: u64,
    pub fixture_sha256: String,
    pub labels_sha256: String,
    pub manifest_sha256: String,
    pub traces: u64,
    pub spans: u64,
}

#[derive(Debug, Serialize)]
pub struct CodeIdentity {
    pub repository: String,
    pub path: PathBuf,
    pub git_head: String,
    pub dirty: bool,
    pub source_tree_sha256: String,
    pub source_tree_exclusions: Vec<&'static str>,
    pub cargo_lock_sha256: Option<String>,
}

const SOURCE_TREE_EXCLUSIONS: &[&str] = &["benchmarks/baselines/"];

pub async fn qualify(
    source_manifest: &Path,
    tier: &str,
    output: &Path,
) -> Result<QualificationReport, Box<dyn Error>> {
    let data = output.join("data");
    let results = output.join("results");
    let workspace = output.join("workspace");
    if workspace.exists() {
        return Err(format!(
            "qualification workspace already exists: {}; choose a fresh output directory",
            workspace.display()
        )
        .into());
    }
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&results)?;
    let source = crate::fetch::fetch_source(source_manifest, &data).await?;
    let fixture = build_fixture(source_manifest, &source, tier, &data)?;
    let repeated = build_fixture(
        source_manifest,
        &source,
        tier,
        &data.join("determinism-check"),
    )?;
    if fixture.fixture_sha256 != repeated.fixture_sha256
        || fixture.labels_sha256 != repeated.labels_sha256
        || fixture.manifest_sha256 != repeated.manifest_sha256
        || fixture.traces != repeated.traces
        || fixture.spans != repeated.spans
    {
        return Err("repeated fixture builds produced different canonical artifacts".into());
    }
    let determinism = FixtureDeterminismReport {
        schema_version: "perseval.benchmark_fixture_determinism.v1",
        repeated_builds: 2,
        fixture_sha256: fixture.fixture_sha256.clone(),
        labels_sha256: fixture.labels_sha256.clone(),
        manifest_sha256: fixture.manifest_sha256.clone(),
        traces: fixture.traces,
        spans: fixture.spans,
    };

    let mut config = PersevalConfigV1 {
        workspace_id: "default".into(),
        workspace_dir: workspace.clone(),
        ..PersevalConfigV1::default()
    };
    config.otlp.enabled = true;
    config.otlp.bind_addr = "127.0.0.1:0".parse()?;
    config.lifecycle.idle_ms = 100;
    config.lifecycle.finalization_grace_ms = 50;
    config.lifecycle.sweep_ms = 25;
    let runtime_config = config.clone();
    let rss_monitor = RssMonitor::start();
    let runtime = ServiceRuntime::start_embedded(config)?;
    let endpoint_result = (|| -> Result<String, Box<dyn Error>> {
        Ok(runtime
            .live()
            .ok_or("embedded benchmark runtime did not expose its live service")?
            .source_health()?
            .effective_address
            .ok_or("embedded benchmark runtime did not expose its OTLP address")?)
    })();
    let endpoint = match endpoint_result {
        Ok(endpoint) => endpoint,
        Err(error) => {
            runtime.shutdown();
            drop(rss_monitor);
            return Err(error);
        }
    };
    let replay_result = replay(&ReplayOptions::new(
        format!("http://{endpoint}"),
        fixture.fixture.clone(),
        format!("perseval-swesmith-{tier}"),
    ))
    .await;
    let replay = match replay_result {
        Ok(report) => report,
        Err(error) => {
            runtime.shutdown();
            drop(rss_monitor);
            return Err(error);
        }
    };
    let ready_result = wait_until_ready(&runtime, replay.traces).await;
    runtime.shutdown();
    let catchup = ready_result?;
    let peak_rss_bytes = rss_monitor.finish();
    drop(runtime);

    let replay_report = results.join("replay.json");
    let score_report = results.join("score.json");
    let profile_report = results.join("profile.json");
    write_json_report(&replay, &replay_report)?;
    let isolation = audit_workspace(&workspace)?;
    write_json_report(&isolation, &results.join("isolation.json"))?;
    let score = score_workspace(&workspace, &fixture.labels, "otlp-local")?;
    write_json_report(&score, &score_report)?;
    let profile = profile_workspace(&workspace, Some(&replay_report))?;
    write_json_report(&profile, &profile_report)?;
    validate_profile_coverage(&profile)?;
    let explained_wall_ratio = profile
        .replay_accounting
        .as_ref()
        .ok_or("qualification profile did not produce replay accounting")?
        .explained_wall_ratio;
    if explained_wall_ratio < 0.95 {
        return Err(format!(
            "qualification failed REC-005: named stages explain only {:.1}% of replay wall time",
            explained_wall_ratio * 100.0
        )
        .into());
    }
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("benchmark crate is not nested under the Perseval repository")?;
    let traces_repository = repository
        .parent()
        .ok_or("Perseval repository has no parent directory")?
        .join("traces-to-evals");
    let code = vec![
        code_identity("perseval", repository)?,
        code_identity("traces-to-evals", &traces_repository)?,
    ];

    let report = QualificationReport {
        schema_version: "perseval.benchmark_qualification.v4",
        tier: tier.into(),
        fixture,
        determinism,
        replay,
        catchup,
        isolation,
        score,
        profile,
        peak_rss_bytes,
        runtime_config,
        code,
        workspace,
        replay_report,
        score_report,
        profile_report,
    };
    write_json_report(&report, &results.join("qualification.json"))?;
    validate_performance_gates(&report)?;
    Ok(report)
}

fn validate_performance_gates(report: &QualificationReport) -> Result<(), Box<dyn Error>> {
    let mut failures = Vec::new();
    if report.replay.rejected_spans != 0 {
        failures.push(format!(
            "rejected_spans={} (required 0)",
            report.replay.rejected_spans
        ));
    }
    if report.replay.elapsed_ms >= MAX_REPLAY_MS {
        failures.push(format!(
            "replay_ms={:.3} (required <{MAX_REPLAY_MS:.0})",
            report.replay.elapsed_ms
        ));
    }
    if report.replay.acknowledgement_latency_ms.samples == 0 {
        failures.push("acknowledgement latency has no samples".into());
    } else if report.replay.acknowledgement_latency_ms.p95 >= MAX_ACKNOWLEDGEMENT_P95_MS {
        failures.push(format!(
            "acknowledgement_p95_ms={:.3} (required <{MAX_ACKNOWLEDGEMENT_P95_MS:.0})",
            report.replay.acknowledgement_latency_ms.p95
        ));
    }
    if report.catchup.projection_catchup_ms >= MAX_PROJECTION_CATCHUP_MS {
        failures.push(format!(
            "projection_catchup_ms={:.3} (required <{MAX_PROJECTION_CATCHUP_MS:.0})",
            report.catchup.projection_catchup_ms
        ));
    }
    if report.catchup.analysis_after_projection_ms >= MAX_ANALYSIS_AFTER_PROJECTION_MS {
        failures.push(format!(
            "analysis_after_projection_ms={:.3} (required <{MAX_ANALYSIS_AFTER_PROJECTION_MS:.0})",
            report.catchup.analysis_after_projection_ms
        ));
    }
    if report.peak_rss_bytes >= MAX_PEAK_RSS_BYTES {
        failures.push(format!(
            "peak_rss_bytes={} (required <{MAX_PEAK_RSS_BYTES})",
            report.peak_rss_bytes
        ));
    }
    if report.profile.pipeline.feature_similarity_models_built > MAX_COHORT_MODELS_PER_BURST {
        failures.push(format!(
            "feature_similarity_models_built={} (required <={MAX_COHORT_MODELS_PER_BURST})",
            report.profile.pipeline.feature_similarity_models_built
        ));
    }
    if report.isolation.violations != 0 {
        failures.push(format!(
            "label_isolation_violations={} (required 0)",
            report.isolation.violations
        ));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "qualification performance/integrity gates failed:\n- {}",
            failures.join("\n- ")
        )
        .into())
    }
}

fn validate_profile_coverage(profile: &WorkspaceProfile) -> Result<(), Box<dyn Error>> {
    let required = [
        "decode",
        "journal_build",
        "payload_blob_durability",
        "raw_blob_durability",
        "normalized_blob_durability",
        "journal_commit",
        "durable_acknowledgement",
        "projection_deserialization",
        "projection",
        "topology",
        "analysis_projection",
        "normalization",
        "detection",
        "analysis_commit",
    ];
    let mut missing = required
        .into_iter()
        .filter(|stage| !profile.instrumented_stage_ms.contains_key(*stage))
        .collect::<Vec<_>>();
    if profile.pipeline.feature_similarity_models_built > 0 {
        missing.extend(
            [
                "cohort_projection",
                "cohort_embedding",
                "cohort_fit",
                "cohort_assignment",
                "cohort_commit",
            ]
            .into_iter()
            .filter(|stage| !profile.instrumented_stage_ms.contains_key(*stage)),
        );
    }
    if !missing.is_empty() {
        return Err(format!(
            "qualification profile is missing required pipeline stages: {}",
            missing.join(", ")
        )
        .into());
    }
    for query in ["list_runs_200", "list_failure_groups_200"] {
        if !profile.query_ms.contains_key(query) {
            return Err(
                format!("qualification profile is missing required query timing {query}").into(),
            );
        }
    }
    Ok(())
}

struct RssMonitor {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<u64>>,
}

impl RssMonitor {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let pid = std::process::id().to_string();
        let thread = thread::spawn(move || {
            let mut peak_kib = 0_u64;
            while !thread_stop.load(Ordering::Acquire) {
                if let Ok(output) = Command::new("ps").args(["-o", "rss=", "-p", &pid]).output()
                    && output.status.success()
                {
                    peak_kib = peak_kib.max(
                        String::from_utf8_lossy(&output.stdout)
                            .trim()
                            .parse::<u64>()
                            .unwrap_or_default(),
                    );
                }
                thread::sleep(Duration::from_millis(250));
            }
            peak_kib.saturating_mul(1_024)
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }

    fn finish(mut self) -> u64 {
        self.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .and_then(|thread| thread.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for RssMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn code_identity(repository: &str, path: &Path) -> Result<CodeIdentity, Box<dyn Error>> {
    let output = Command::new("git")
        .args([
            "-C",
            path.to_str().ok_or("repository path is not UTF-8")?,
            "rev-parse",
            "HEAD",
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!("could not identify {repository} git revision").into());
    }
    let status = Command::new("git")
        .args([
            "-C",
            path.to_str().ok_or("repository path is not UTF-8")?,
            "status",
            "--porcelain",
        ])
        .output()?;
    if !status.status.success() {
        return Err(format!("could not inspect {repository} worktree status").into());
    }
    let lock = path.join("Cargo.lock");
    let listed = Command::new("git")
        .args([
            "-C",
            path.to_str().ok_or("repository path is not UTF-8")?,
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()?;
    if !listed.status.success() {
        return Err(format!("could not enumerate {repository} source tree").into());
    }
    let mut files = String::from_utf8(listed.stdout)?
        .split('\0')
        .filter(|file| !file.is_empty())
        .filter(|file| {
            !SOURCE_TREE_EXCLUSIONS
                .iter()
                .any(|excluded| file.starts_with(excluded))
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    files.sort_unstable();
    let mut tree_digest = Sha256::new();
    for file in files {
        let source = path.join(&file);
        if source.is_file() {
            tree_digest.update(file.as_bytes());
            tree_digest.update(b"\0");
            tree_digest.update(sha256_file(&source)?.as_bytes());
            tree_digest.update(b"\n");
        }
    }
    Ok(CodeIdentity {
        repository: repository.into(),
        path: path.to_path_buf(),
        git_head: String::from_utf8(output.stdout)?.trim().into(),
        dirty: !status.stdout.is_empty(),
        source_tree_sha256: hex::encode(tree_digest.finalize()),
        source_tree_exclusions: SOURCE_TREE_EXCLUSIONS.to_vec(),
        cargo_lock_sha256: lock.is_file().then(|| sha256_file(&lock)).transpose()?,
    })
}

async fn wait_until_ready(
    runtime: &ServiceRuntime,
    expected_traces: u64,
) -> Result<CatchupReport, Box<dyn Error>> {
    let live = runtime
        .live()
        .ok_or("embedded benchmark runtime stopped unexpectedly")?;
    let started = Instant::now();
    let mut projection_catchup_ms = None;
    let mut analysis_ready_ms = None;
    let mut topology_ready_ms = None;
    loop {
        let health = live.source_health()?;
        let run_count = live.run_count()?;
        let mut ready_count = 0_u64;
        let mut offset = 0_u64;
        while offset < run_count {
            let page = live.list_runs(offset, 200)?;
            if page.is_empty() {
                break;
            }
            ready_count = ready_count.saturating_add(
                page.iter()
                    .filter(|run| run.analysis_status == AnalysisStatus::Ready)
                    .count() as u64,
            );
            offset = offset.saturating_add(page.len() as u64);
        }
        let analysis_ready = run_count == expected_traces
            && ready_count == expected_traces
            && health.journal_lag == 0
            && health.projection_lag == 0
            && health.analysis_pending == 0
            && health.analysis_running == 0;
        if projection_catchup_ms.is_none()
            && run_count == expected_traces
            && health.journal_lag == 0
            && health.projection_lag == 0
        {
            projection_catchup_ms = Some(started.elapsed().as_secs_f64() * 1_000.0);
        }
        if analysis_ready && analysis_ready_ms.is_none() {
            analysis_ready_ms = Some(started.elapsed().as_secs_f64() * 1_000.0);
        }
        if analysis_ready
            && health.topology_pending == 0
            && health.topology_running == 0
            && topology_ready_ms.is_none()
        {
            topology_ready_ms = Some(started.elapsed().as_secs_f64() * 1_000.0);
        }
        if let (Some(analysis_ready_ms), Some(topology_ready_ms)) =
            (analysis_ready_ms, topology_ready_ms)
        {
            let fully_ready_ms = started.elapsed().as_secs_f64() * 1_000.0;
            let projection_catchup_ms = projection_catchup_ms.unwrap_or(analysis_ready_ms);
            return Ok(CatchupReport {
                schema_version: "perseval.benchmark_catchup_report.v2",
                projection_catchup_ms,
                analysis_ready_ms,
                analysis_after_projection_ms: (analysis_ready_ms - projection_catchup_ms).max(0.0),
                topology_ready_ms,
                fully_ready_ms,
            });
        }
        if started.elapsed() >= QUALIFICATION_TIMEOUT {
            return Err(format!(
                "qualification timed out: runs={}/{expected_traces}, journal_lag={}, projection_lag={}, analysis_pending={}, analysis_running={}, topology_pending={}, topology_running={}",
                run_count,
                health.journal_lag,
                health.projection_lag,
                health.analysis_pending,
                health.analysis_running,
                health.topology_pending,
                health.topology_running
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
