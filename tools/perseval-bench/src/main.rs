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
mod manifest;
mod profile;
mod qualify;
mod reanalyze;
mod replay;
mod score;

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
        "usage:\n  {program} fetch SOURCE_MANIFEST.json OUTPUT_DIRECTORY\n  {program} prepare SOURCE_MANIFEST.json TIER OUTPUT_DIRECTORY\n  {program} qualify SOURCE_MANIFEST.json TIER OUTPUT_DIRECTORY\n  {program} inspect-source SOURCE.parquet\n  {program} build-fixture SOURCE_MANIFEST.json SOURCE.parquet TIER OUTPUT_DIRECTORY\n  {program} build-detector-fixture OUTPUT_DIRECTORY\n  {program} guard FIXTURE.jsonl\n  {program} audit-isolation WORKSPACE\n  {program} replay ENDPOINT FIXTURE.jsonl PROJECT [BATCH_SIZE]\n  {program} reanalyze WORKSPACE [TIMEOUT_SECONDS]\n  {program} score WORKSPACE LABELS.jsonl OUTPUT.json [SOURCE_ID]\n  {program} score-assessments ASSESSMENT_EXPORT.json LABELS.jsonl OUTPUT.json\n  {program} score-detectors FIXTURE.jsonl LABELS.jsonl SPLIT OUTPUT.json\n  {program} score-default-detectors BEHAVIOR_FIXTURE.jsonl DETECTOR_LABELS.jsonl SPLIT OUTPUT.json\n  {program} profile WORKSPACE OUTPUT.json [REPLAY_REPORT.json]"
    )
}
