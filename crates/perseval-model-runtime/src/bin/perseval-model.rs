use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use perseval_model_runtime::{TaskCompletionOnnxRuntime, load_parity_fixture, verify_artifact};
use serde::Serialize;
use traces_to_evals::{CompactTaskCompletionProjectionV1, TaskCompletionTrainingRecordV1};

#[derive(Debug, Serialize)]
struct TrainingRecordReport<'a> {
    input: &'a Path,
    output: &'a Path,
    records: usize,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os();
    let program = args
        .next()
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| "perseval-model".into());
    let command = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| usage(&program))?;
    match command.as_str() {
        "verify" => {
            let directory = args
                .next()
                .ok_or_else(|| "verify requires an artifact directory".to_string())?;
            ensure_no_more_args(args, &program)?;
            let (_, report) = verify_artifact(Path::new(&directory))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "parity" => {
            let directory = args
                .next()
                .ok_or_else(|| "parity requires an artifact directory".to_string())?;
            ensure_no_more_args(args, &program)?;
            let directory = Path::new(&directory);
            let mut runtime = TaskCompletionOnnxRuntime::load(directory)?;
            let fixture = load_parity_fixture(directory, runtime.manifest())?;
            let report = runtime.run_parity(&fixture)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "training-records" => {
            let input = args
                .next()
                .ok_or_else(|| "training-records requires projection JSONL".to_string())?;
            let output = args
                .next()
                .ok_or_else(|| "training-records requires output JSONL".to_string())?;
            ensure_no_more_args(args, &program)?;
            convert_training_records(Path::new(&input), Path::new(&output))
        }
        _ => Err(format!("unknown command {command:?}\n{}", usage(&program)).into()),
    }
}

fn ensure_no_more_args(
    mut args: impl Iterator<Item = std::ffi::OsString>,
    program: &str,
) -> Result<(), Box<dyn Error>> {
    if args.next().is_some() {
        return Err(usage(program).into());
    }
    Ok(())
}

fn convert_training_records(input: &Path, output: &Path) -> Result<(), Box<dyn Error>> {
    if input == output {
        return Err("input and output paths must be distinct".into());
    }
    let reader = BufReader::new(File::open(input)?);
    let mut records = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let projection: CompactTaskCompletionProjectionV1 = serde_json::from_str(&line)
            .map_err(|error| format!("invalid projection on line {}: {error}", index + 1))?;
        let record = TaskCompletionTrainingRecordV1::from_projection(&projection)
            .map_err(|error| format!("invalid projection on line {}: {error}", index + 1))?;
        records.push(record);
    }

    let mut writer = BufWriter::new(File::create(output)?);
    for record in &records {
        serde_json::to_writer(&mut writer, record)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;

    println!(
        "{}",
        serde_json::to_string_pretty(&TrainingRecordReport {
            input,
            output,
            records: records.len(),
        })?
    );
    Ok(())
}

fn usage(program: &str) -> String {
    format!(
        "usage:\n  {program} verify ARTIFACT_DIRECTORY\n  {program} parity ARTIFACT_DIRECTORY\n  {program} training-records PROJECTIONS.jsonl OUTPUT.jsonl"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_produces_an_empty_record_file() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("projections.jsonl");
        let output = directory.path().join("records.jsonl");
        std::fs::write(&input, "\n").unwrap();

        convert_training_records(&input, &output).unwrap();

        assert_eq!(std::fs::read_to_string(output).unwrap(), "");
    }

    #[test]
    fn conversion_rejects_partial_invalid_input_before_writing() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("projections.jsonl");
        let output = directory.path().join("records.jsonl");
        std::fs::write(&input, "{}\n").unwrap();

        assert!(convert_training_records(&input, &output).is_err());
        assert!(!output.exists());
    }
}
