use std::env;
use std::error::Error;
use std::path::Path;

use duckdb::Connection;
use serde::Serialize;

mod assessment_score;
mod default_detector_fixture;
mod default_detector_score;
mod detector_score;
mod fetch;
mod fixture;
mod guard;
mod isolation;
mod local_chat;
mod manifest;
mod profile;
mod qualify;
mod reanalyze;
mod replay;
mod score;
mod task_completion;
mod task_completion_calibrator;
mod task_completion_compact;
mod task_completion_features;
mod task_completion_models;

#[derive(Debug, Serialize)]
struct SourceColumn {
    name: String,
    data_type: String,
    nullable: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os();
    let program = args
        .next()
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| "perseval-bench".into());
    let Some(command) = args.next().and_then(|value| value.into_string().ok()) else {
        return Err(usage(&program).into());
    };
    match command.as_str() {
        "fetch" => {
            let manifest = args
                .next()
                .ok_or_else(|| "fetch requires a source manifest path".to_string())?;
            let output_directory = args
                .next()
                .ok_or_else(|| "fetch requires an output directory".to_string())?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            fetch::fetch_source(Path::new(&manifest), Path::new(&output_directory)).await?;
            Ok(())
        }
        "inspect-source" => {
            let source = args
                .next()
                .ok_or_else(|| "inspect-source requires a Parquet path".to_string())?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            inspect_source(Path::new(&source))
        }
        "build-fixture" => {
            let manifest = args
                .next()
                .ok_or_else(|| "build-fixture requires a source manifest".to_string())?;
            let source = args
                .next()
                .ok_or_else(|| "build-fixture requires a Parquet source".to_string())?;
            let tier = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "build-fixture requires a tier name".to_string())?;
            let output = args
                .next()
                .ok_or_else(|| "build-fixture requires an output directory".to_string())?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = fixture::build_fixture(
                Path::new(&manifest),
                Path::new(&source),
                &tier,
                Path::new(&output),
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "build-detector-fixture" => {
            let output = args
                .next()
                .ok_or_else(|| "build-detector-fixture requires an output directory".to_string())?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report =
                default_detector_fixture::build_default_detector_fixture(Path::new(&output))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "prepare" => {
            let manifest = args
                .next()
                .ok_or_else(|| "prepare requires a source manifest".to_string())?;
            let tier = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "prepare requires a tier name".to_string())?;
            let output = args
                .next()
                .ok_or_else(|| "prepare requires an output directory".to_string())?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let output = Path::new(&output);
            let source = fetch::fetch_source(Path::new(&manifest), output).await?;
            let report = fixture::build_fixture(Path::new(&manifest), &source, &tier, output)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "qualify" => {
            let manifest = args
                .next()
                .ok_or_else(|| "qualify requires a source manifest".to_string())?;
            let tier = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "qualify requires a tier name".to_string())?;
            let output = args
                .next()
                .ok_or_else(|| "qualify requires an output directory".to_string())?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = qualify::qualify(Path::new(&manifest), &tier, Path::new(&output)).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "guard" => {
            let fixture = args
                .next()
                .ok_or_else(|| "guard requires a fixture path".to_string())?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let summary = guard::guard_fixture(Path::new(&fixture))?;
            println!(
                "guard passed: traces={} spans={} inspected_facts={}",
                summary.traces, summary.spans, summary.inspected_facts
            );
            Ok(())
        }
        "audit-isolation" => {
            let workspace = args
                .next()
                .ok_or_else(|| "audit-isolation requires a workspace directory".to_string())?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = isolation::audit_workspace(Path::new(&workspace))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "replay" => {
            let endpoint = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "replay requires an OTLP endpoint".to_string())?;
            let fixture = args
                .next()
                .ok_or_else(|| "replay requires a fixture path".to_string())?;
            let project = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "replay requires a project name".to_string())?;
            let mut options = replay::ReplayOptions::new(endpoint, fixture.into(), project);
            if let Some(batch_size) = args.next() {
                options.batch_size = batch_size
                    .into_string()
                    .map_err(|_| "batch size is not UTF-8")?
                    .parse()?;
            }
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = replay::replay(&options).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "reanalyze" => {
            let workspace = args
                .next()
                .ok_or_else(|| "reanalyze requires a workspace directory".to_string())?;
            let timeout_seconds = args
                .next()
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| "timeout is not UTF-8".to_string())?
                        .parse::<u64>()
                        .map_err(|error| error.to_string())
                })
                .transpose()?
                .unwrap_or(20 * 60);
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = reanalyze::reanalyze_workspace(
                Path::new(&workspace),
                std::time::Duration::from_secs(timeout_seconds),
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "score" => {
            let workspace = args
                .next()
                .ok_or_else(|| "score requires a workspace directory".to_string())?;
            let labels = args
                .next()
                .ok_or_else(|| "score requires a label sidecar".to_string())?;
            let output = args
                .next()
                .ok_or_else(|| "score requires an output report path".to_string())?;
            let source_id = args
                .next()
                .and_then(|value| value.into_string().ok())
                .unwrap_or_else(|| "otlp-local".into());
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report =
                score::score_workspace(Path::new(&workspace), Path::new(&labels), &source_id)?;
            score::write_json_report(&report, Path::new(&output))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "score-assessments" => {
            let export = args
                .next()
                .ok_or_else(|| "score-assessments requires an assessment export".to_string())?;
            let labels = args
                .next()
                .ok_or_else(|| "score-assessments requires a label sidecar".to_string())?;
            let output = args
                .next()
                .ok_or_else(|| "score-assessments requires an output report path".to_string())?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report =
                assessment_score::score_assessments(Path::new(&export), Path::new(&labels))?;
            score::write_json_report(&report, Path::new(&output))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "score-detectors" => {
            let fixture = args
                .next()
                .ok_or_else(|| "score-detectors requires a fixture path".to_string())?;
            let labels = args
                .next()
                .ok_or_else(|| "score-detectors requires a label sidecar".to_string())?;
            let split = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "score-detectors requires a split name".to_string())?;
            let output = args
                .next()
                .ok_or_else(|| "score-detectors requires an output report path".to_string())?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report =
                detector_score::score_detectors(Path::new(&fixture), Path::new(&labels), &split)?;
            score::write_json_report(&report, Path::new(&output))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "score-default-detectors" => {
            let fixture = args.next().ok_or_else(|| {
                "score-default-detectors requires a behavior fixture path".to_string()
            })?;
            let labels = args.next().ok_or_else(|| {
                "score-default-detectors requires a detector label sidecar".to_string()
            })?;
            let split = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "score-default-detectors requires a split name".to_string())?;
            let output = args.next().ok_or_else(|| {
                "score-default-detectors requires an output report path".to_string()
            })?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = default_detector_score::score_default_detectors(
                Path::new(&fixture),
                Path::new(&labels),
                &split,
            )?;
            score::write_json_report(&report, Path::new(&output))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "task-completion-run" => {
            let suite = args
                .next()
                .ok_or_else(|| "task-completion-run requires a trace suite".to_string())?;
            let labels = args
                .next()
                .ok_or_else(|| "task-completion-run requires a label sidecar".to_string())?;
            let split = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "task-completion-run requires a split".to_string())?;
            let output = args
                .next()
                .ok_or_else(|| "task-completion-run requires an output directory".to_string())?;
            let model = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "task-completion-run requires a model".to_string())?;
            let profile = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "task-completion-run requires a rubric profile".to_string())?;
            let concurrency = args
                .next()
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| "concurrency is not UTF-8".to_string())?
                        .parse::<usize>()
                        .map_err(|error| error.to_string())
                })
                .transpose()?
                .unwrap_or(4);
            let limit = args
                .next()
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| "limit is not UTF-8".to_string())?
                        .parse::<usize>()
                        .map_err(|error| error.to_string())
                })
                .transpose()?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = task_completion::run(task_completion::RunOptions {
                suite: Path::new(&suite),
                labels: Path::new(&labels),
                split: &split,
                output: Path::new(&output),
                model: &model,
                profile: &profile,
                concurrency,
                limit,
            })
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "task-completion-score" => {
            let recall = args
                .next()
                .ok_or_else(|| "task-completion-score requires recall-judge results".to_string())?;
            let specificity = args.next().ok_or_else(|| {
                "task-completion-score requires specificity-judge results".to_string()
            })?;
            let labels = args
                .next()
                .ok_or_else(|| "task-completion-score requires a label sidecar".to_string())?;
            let output = args
                .next()
                .ok_or_else(|| "task-completion-score requires an output report".to_string())?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = task_completion::score(
                Path::new(&recall),
                Path::new(&specificity),
                Path::new(&labels),
            )?;
            score::write_json_report(&report, Path::new(&output))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "task-completion-zero-shot-score" => {
            let results = args.next().ok_or_else(|| {
                "task-completion-zero-shot-score requires evaluator results".to_string()
            })?;
            let labels = args.next().ok_or_else(|| {
                "task-completion-zero-shot-score requires a label sidecar".to_string()
            })?;
            let output = args.next().ok_or_else(|| {
                "task-completion-zero-shot-score requires an output report".to_string()
            })?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = task_completion::score_zero_shot(Path::new(&results), Path::new(&labels))?;
            score::write_json_report(&report, Path::new(&output))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "task-completion-project" => {
            let suite = args
                .next()
                .ok_or_else(|| "task-completion-project requires a trace suite".to_string())?;
            let labels = args
                .next()
                .ok_or_else(|| "task-completion-project requires a label sidecar".to_string())?;
            let split = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "task-completion-project requires a split".to_string())?;
            let output = args
                .next()
                .ok_or_else(|| "task-completion-project requires an output path".to_string())?;
            let variant = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "task-completion-project requires a variant".to_string())?;
            let limit = args
                .next()
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| "limit is not UTF-8".to_string())?
                        .parse::<usize>()
                        .map_err(|error| error.to_string())
                })
                .transpose()?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = task_completion_compact::project_suite(
                Path::new(&suite),
                Path::new(&labels),
                &split,
                Path::new(&output),
                &variant,
                limit,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "task-completion-smollm-run" => {
            let projections = args.next().ok_or_else(|| {
                "task-completion-smollm-run requires compact projections".to_string()
            })?;
            let output = args
                .next()
                .ok_or_else(|| "task-completion-smollm-run requires an output".to_string())?;
            let model_id = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "task-completion-smollm-run requires a model id".to_string())?;
            let model_hash = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "task-completion-smollm-run requires a model hash".to_string())?;
            let threshold = args
                .next()
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| "threshold is not UTF-8".to_string())?
                        .parse::<f64>()
                        .map_err(|error| error.to_string())
                })
                .transpose()?
                .unwrap_or(0.5);
            let concurrency = args
                .next()
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| "concurrency is not UTF-8".to_string())?
                        .parse::<usize>()
                        .map_err(|error| error.to_string())
                })
                .transpose()?
                .unwrap_or(4);
            let limit = args
                .next()
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| "limit is not UTF-8".to_string())?
                        .parse::<usize>()
                        .map_err(|error| error.to_string())
                })
                .transpose()?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = task_completion_models::run_smollm(
                Path::new(&projections),
                Path::new(&output),
                &model_id,
                &model_hash,
                threshold,
                concurrency,
                limit,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "task-completion-openai-run" => {
            let projections = args.next().ok_or_else(|| {
                "task-completion-openai-run requires compact projections".to_string()
            })?;
            let output = args
                .next()
                .ok_or_else(|| "task-completion-openai-run requires an output".to_string())?;
            let model_id = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "task-completion-openai-run requires a model id".to_string())?;
            let concurrency = args
                .next()
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| "concurrency is not UTF-8".to_string())?
                        .parse::<usize>()
                        .map_err(|error| error.to_string())
                })
                .transpose()?
                .unwrap_or(4);
            let limit = args
                .next()
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| "limit is not UTF-8".to_string())?
                        .parse::<usize>()
                        .map_err(|error| error.to_string())
                })
                .transpose()?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = task_completion_models::run_openai(
                Path::new(&projections),
                Path::new(&output),
                &model_id,
                concurrency,
                limit,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "task-completion-modernbert-nli-run" => {
            let projections = args.next().ok_or_else(|| {
                "task-completion-modernbert-nli-run requires compact projections".to_string()
            })?;
            let output = args.next().ok_or_else(|| {
                "task-completion-modernbert-nli-run requires an output".to_string()
            })?;
            let model_path = args.next().ok_or_else(|| {
                "task-completion-modernbert-nli-run requires an ONNX model".to_string()
            })?;
            let tokenizer_path = args.next().ok_or_else(|| {
                "task-completion-modernbert-nli-run requires a tokenizer".to_string()
            })?;
            let model_id = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| {
                    "task-completion-modernbert-nli-run requires a model id".to_string()
                })?;
            let model_hash = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| {
                    "task-completion-modernbert-nli-run requires a model hash".to_string()
                })?;
            let tokenizer_hash = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| {
                    "task-completion-modernbert-nli-run requires a tokenizer hash".to_string()
                })?;
            let neutral_policy = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| {
                    "task-completion-modernbert-nli-run requires a neutral policy".to_string()
                })?;
            let threshold = args
                .next()
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| "threshold is not UTF-8".to_string())?
                        .parse::<f64>()
                        .map_err(|error| error.to_string())
                })
                .transpose()?
                .unwrap_or(0.5);
            let limit = args
                .next()
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| "limit is not UTF-8".to_string())?
                        .parse::<usize>()
                        .map_err(|error| error.to_string())
                })
                .transpose()?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = task_completion_models::run_modernbert_nli(
                Path::new(&projections),
                Path::new(&output),
                Path::new(&model_path),
                Path::new(&tokenizer_path),
                &model_id,
                &model_hash,
                &tokenizer_hash,
                &neutral_policy,
                threshold,
                limit,
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "task-completion-semantic-nli-run" => {
            let projections = args.next().ok_or_else(|| {
                "task-completion-semantic-nli-run requires compact projections".to_string()
            })?;
            let output = args
                .next()
                .ok_or_else(|| "task-completion-semantic-nli-run requires an output".to_string())?;
            let model_path = args.next().ok_or_else(|| {
                "task-completion-semantic-nli-run requires an ONNX model".to_string()
            })?;
            let tokenizer_path = args.next().ok_or_else(|| {
                "task-completion-semantic-nli-run requires a tokenizer".to_string()
            })?;
            let model_id = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| {
                    "task-completion-semantic-nli-run requires a model id".to_string()
                })?;
            let model_hash = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| {
                    "task-completion-semantic-nli-run requires a model hash".to_string()
                })?;
            let tokenizer_hash = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| {
                    "task-completion-semantic-nli-run requires a tokenizer hash".to_string()
                })?;
            let label_order = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| {
                    "task-completion-semantic-nli-run requires a label order".to_string()
                })?;
            let limit = args
                .next()
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| "limit is not UTF-8".to_string())?
                        .parse::<usize>()
                        .map_err(|error| error.to_string())
                })
                .transpose()?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = task_completion_models::run_semantic_nli(
                Path::new(&projections),
                Path::new(&output),
                Path::new(&model_path),
                Path::new(&tokenizer_path),
                &model_id,
                &model_hash,
                &tokenizer_hash,
                &label_order,
                limit,
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "task-completion-binary-score" | "task-completion-binary-calibrate" => {
            let results = args
                .next()
                .ok_or_else(|| format!("{command} requires binary model results"))?;
            let labels = args
                .next()
                .ok_or_else(|| format!("{command} requires resolution labels"))?;
            let split = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| format!("{command} requires a split"))?;
            let output = args
                .next()
                .ok_or_else(|| format!("{command} requires an output report"))?;
            let threshold = args
                .next()
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| "threshold is not UTF-8".to_string())?
                        .parse::<f64>()
                        .map_err(|error| error.to_string())
                })
                .transpose()?;
            if args.next().is_some()
                || (command == "task-completion-binary-calibrate" && threshold.is_some())
            {
                return Err(usage(&program).into());
            }
            let report = if command == "task-completion-binary-calibrate" {
                task_completion_models::calibrate(Path::new(&results), Path::new(&labels), &split)?
            } else {
                task_completion_models::score(
                    Path::new(&results),
                    Path::new(&labels),
                    &split,
                    threshold,
                )?
            };
            task_completion_models::write_report(&report, Path::new(&output))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "task-completion-cpu-calibrate" => {
            let labels = args
                .next()
                .ok_or_else(|| "task-completion-cpu-calibrate requires labels".to_string())?;
            let split = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| "task-completion-cpu-calibrate requires a split".to_string())?;
            let output = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate requires an output report".to_string()
            })?;
            let goal_final = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate requires goal+final results".to_string()
            })?;
            let mandatory = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate requires mandatory results".to_string()
            })?;
            let mandatory_recovery = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate requires mandatory+recovery results".to_string()
            })?;
            let complete = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate requires complete-projection results".to_string()
            })?;
            let nli = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate requires ModernBERT NLI results".to_string()
            })?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = task_completion_calibrator::calibrate(
                Path::new(&labels),
                &split,
                task_completion_calibrator::LearnedResultPaths {
                    goal_final: Path::new(&goal_final),
                    mandatory: Path::new(&mandatory),
                    mandatory_recovery: Path::new(&mandatory_recovery),
                    complete: Path::new(&complete),
                    nli: Path::new(&nli),
                },
            )?;
            task_completion_calibrator::write_report(&report, Path::new(&output))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "task-completion-cpu-calibrate-single" => {
            let labels = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate-single requires labels".to_string()
            })?;
            let split = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| {
                    "task-completion-cpu-calibrate-single requires a split".to_string()
                })?;
            let output = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate-single requires an output report".to_string()
            })?;
            let results = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate-single requires binary judge results".to_string()
            })?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = task_completion_calibrator::calibrate_single(
                Path::new(&labels),
                &split,
                Path::new(&results),
            )?;
            task_completion_calibrator::write_report(&report, Path::new(&output))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "task-completion-structured-features" => {
            let projections = args.next().ok_or_else(|| {
                "task-completion-structured-features requires projections".to_string()
            })?;
            let results = args.next().ok_or_else(|| {
                "task-completion-structured-features requires binary judge results".to_string()
            })?;
            let output = args.next().ok_or_else(|| {
                "task-completion-structured-features requires an output path".to_string()
            })?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let count = task_completion_features::extract(
                Path::new(&projections),
                Path::new(&results),
                Path::new(&output),
            )?;
            println!("wrote {count} structured task-completion feature records");
            Ok(())
        }
        "task-completion-cpu-calibrate-structured" => {
            let labels = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate-structured requires labels".to_string()
            })?;
            let split = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| {
                    "task-completion-cpu-calibrate-structured requires a split".to_string()
                })?;
            let output = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate-structured requires an output report".to_string()
            })?;
            let features = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate-structured requires feature records".to_string()
            })?;
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = task_completion_calibrator::calibrate_structured(
                Path::new(&labels),
                &split,
                Path::new(&features),
            )?;
            task_completion_calibrator::write_report(&report, Path::new(&output))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "task-completion-cpu-calibrate-semantic" => {
            let labels = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate-semantic requires labels".to_string()
            })?;
            let split = args
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| {
                    "task-completion-cpu-calibrate-semantic requires a split".to_string()
                })?;
            let output = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate-semantic requires an output report".to_string()
            })?;
            let semantic = args.next().ok_or_else(|| {
                "task-completion-cpu-calibrate-semantic requires semantic features".to_string()
            })?;
            let structured = args.next();
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = task_completion_calibrator::calibrate_semantic(
                Path::new(&labels),
                &split,
                Path::new(&semantic),
                structured.as_deref().map(Path::new),
            )?;
            task_completion_calibrator::write_report(&report, Path::new(&output))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "profile" => {
            let workspace = args
                .next()
                .ok_or_else(|| "profile requires a workspace directory".to_string())?;
            let output = args
                .next()
                .ok_or_else(|| "profile requires an output report path".to_string())?;
            let replay_report = args.next();
            if args.next().is_some() {
                return Err(usage(&program).into());
            }
            let report = profile::profile_workspace(
                Path::new(&workspace),
                replay_report.as_deref().map(Path::new),
            )?;
            score::write_json_report(&report, Path::new(&output))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        _ => Err(format!("unknown command {command:?}\n{}", usage(&program)).into()),
    }
}

fn inspect_source(source: &Path) -> Result<(), Box<dyn Error>> {
    if !source.is_file() {
        return Err(format!("source does not exist: {}", source.display()).into());
    }
    let connection = Connection::open_in_memory()?;
    let source = source
        .to_str()
        .ok_or_else(|| format!("source path is not UTF-8: {}", source.display()))?;
    let mut statement = connection.prepare(
        "SELECT column_name, column_type, null FROM (DESCRIBE SELECT * FROM read_parquet(?1))",
    )?;
    let columns = statement
        .query_map([source], |row| {
            Ok(SourceColumn {
                name: row.get(0)?,
                data_type: row.get(1)?,
                nullable: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let row_count: i64 =
        connection.query_row("SELECT count(*) FROM read_parquet(?1)", [source], |row| {
            row.get(0)
        })?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "source": source,
            "rows": row_count,
            "columns": columns,
        }))?
    );
    Ok(())
}

fn usage(program: &str) -> String {
    format!(
        "usage:\n  {program} fetch SOURCE_MANIFEST.json OUTPUT_DIRECTORY\n  {program} prepare SOURCE_MANIFEST.json TIER OUTPUT_DIRECTORY\n  {program} qualify SOURCE_MANIFEST.json TIER OUTPUT_DIRECTORY\n  {program} inspect-source SOURCE.parquet\n  {program} build-fixture SOURCE_MANIFEST.json SOURCE.parquet TIER OUTPUT_DIRECTORY\n  {program} build-detector-fixture OUTPUT_DIRECTORY\n  {program} guard FIXTURE.jsonl\n  {program} audit-isolation WORKSPACE\n  {program} replay ENDPOINT FIXTURE.jsonl PROJECT [BATCH_SIZE]\n  {program} reanalyze WORKSPACE [TIMEOUT_SECONDS]\n  {program} score WORKSPACE LABELS.jsonl OUTPUT.json [SOURCE_ID]\n  {program} score-assessments ASSESSMENT_EXPORT.json LABELS.jsonl OUTPUT.json\n  {program} score-detectors FIXTURE.jsonl LABELS.jsonl SPLIT OUTPUT.json\n  {program} score-default-detectors BEHAVIOR_FIXTURE.jsonl DETECTOR_LABELS.jsonl SPLIT OUTPUT.json\n  {program} task-completion-run TRACE_SUITE.jsonl LABELS.jsonl SPLIT OUTPUT_DIRECTORY MODEL PROFILE [CONCURRENCY] [LIMIT]\n  {program} task-completion-score RECALL_RESULTS SPECIFICITY_RESULTS LABELS.jsonl OUTPUT.json\n  {program} task-completion-zero-shot-score RESULTS LABELS.jsonl OUTPUT.json\n  {program} task-completion-project TRACE_SUITE.jsonl LABELS.jsonl SPLIT OUTPUT.jsonl VARIANT [LIMIT]\n  {program} task-completion-smollm-run PROJECTIONS.jsonl OUTPUT.jsonl MODEL_ID MODEL_HASH [THRESHOLD] [CONCURRENCY] [LIMIT]\n  {program} task-completion-openai-run PROJECTIONS.jsonl OUTPUT.jsonl MODEL_ID [CONCURRENCY] [LIMIT]\n  {program} task-completion-modernbert-nli-run PROJECTIONS.jsonl OUTPUT.jsonl MODEL.onnx TOKENIZER.json MODEL_ID MODEL_HASH TOKENIZER_HASH NEUTRAL_POLICY [THRESHOLD] [LIMIT]\n  {program} task-completion-semantic-nli-run PROJECTIONS.jsonl OUTPUT.jsonl MODEL.onnx TOKENIZER.json MODEL_ID MODEL_HASH TOKENIZER_HASH LABEL_ORDER [LIMIT]\n  {program} task-completion-binary-score RESULTS.jsonl LABELS.jsonl SPLIT OUTPUT.json [THRESHOLD]\n  {program} task-completion-binary-calibrate RESULTS.jsonl LABELS.jsonl SPLIT OUTPUT.json\n  {program} task-completion-cpu-calibrate LABELS.jsonl SPLIT OUTPUT.json GOAL_FINAL.jsonl MANDATORY.jsonl MANDATORY_RECOVERY.jsonl COMPLETE.jsonl NLI.jsonl\n  {program} task-completion-cpu-calibrate-single LABELS.jsonl SPLIT OUTPUT.json RESULTS.jsonl\n  {program} task-completion-structured-features PROJECTIONS.jsonl RESULTS.jsonl OUTPUT.jsonl\n  {program} task-completion-cpu-calibrate-structured LABELS.jsonl SPLIT OUTPUT.json FEATURES.jsonl\n  {program} task-completion-cpu-calibrate-semantic LABELS.jsonl SPLIT OUTPUT.json SEMANTIC.jsonl [STRUCTURED.jsonl]\n  {program} profile WORKSPACE OUTPUT.json [REPLAY_REPORT.json]"
    )
}
