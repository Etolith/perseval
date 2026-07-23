use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use traceeval_contracts::{
    COMPACT_TASK_COMPLETION_PROJECTION_SCHEMA_VERSION, CompactTaskCompletionProjectionV1,
    EvaluationImplementationV1, EvaluatorReleaseSpecV1, LearnedTaskKind,
    TASK_COMPLETION_STRUCTURED_FEATURE_SET_VERSION, TASK_COMPLETION_TRAINING_RECORD_SCHEMA_VERSION,
    TaskCompletionEvidenceFeatureRecordV1,
};

pub const TASK_COMPLETION_MODEL_MANIFEST_SCHEMA_VERSION: &str =
    "perseval.task_completion_model_manifest.v1";
pub const TASK_COMPLETION_PARITY_FIXTURE_SCHEMA_VERSION: &str =
    "perseval.task_completion_onnx_parity.v1";
pub const TASK_COMPLETION_ONNX_RUNTIME_VERSION: &str = "perseval.task_completion_onnx.v1";
pub const TASK_COMPLETION_STRUCTURED_INPUT_NAME: &str = "structured_features";

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("artifact I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("artifact JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ONNX Runtime failed: {0}")]
    Ort(#[from] ort::Error),
    #[error("invalid task-completion artifact: {0}")]
    InvalidArtifact(String),
}

pub type Result<T> = std::result::Result<T, RuntimeError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactFileV1 {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TensorElementTypeV1 {
    F32,
    I64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TensorSpecV1 {
    pub name: String,
    pub element_type: TensorElementTypeV1,
    pub dimensions: Vec<Option<usize>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationManifestV1 {
    pub version: String,
    pub threshold_complete: f32,
    pub temperature: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionOutputV1 {
    pub output_name: String,
    pub incomplete_logit_index: usize,
    pub complete_logit_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLineageV1 {
    pub base_model_id: String,
    pub base_model_revision: String,
    pub projector_schema_version: String,
    pub projector_version: String,
    pub training_record_schema_version: String,
    pub feature_set_version: String,
    pub dataset_version: String,
    pub dataset_sha256: String,
    pub training_version: String,
    pub calibration_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskCompletionModelManifestV1 {
    pub schema_version: String,
    pub model_id: String,
    pub model_file: ArtifactFileV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokenizer_file: Option<ArtifactFileV1>,
    pub parity_file: ArtifactFileV1,
    pub parity_absolute_tolerance: f32,
    pub inputs: Vec<TensorSpecV1>,
    pub outputs: Vec<TensorSpecV1>,
    pub decision_output: DecisionOutputV1,
    pub calibration: CalibrationManifestV1,
    pub lineage: ModelLineageV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskCompletionLabelV1 {
    Incomplete,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskCompletionDecisionV1 {
    pub model_id: String,
    pub label: TaskCompletionLabelV1,
    pub raw_logit_margin: f32,
    pub calibrated_probability_complete: f32,
    pub threshold_complete: f32,
    pub calibration_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactVerificationReportV1 {
    pub model_id: String,
    pub model_path: PathBuf,
    pub parity_path: PathBuf,
    pub tokenizer_path: Option<PathBuf>,
    pub verified_files: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TensorFixtureV1 {
    pub name: String,
    pub element_type: TensorElementTypeV1,
    pub shape: Vec<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub f32_values: Vec<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub i64_values: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectedTensorV1 {
    pub name: String,
    pub shape: Vec<usize>,
    pub f32_values: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParityCaseV1 {
    pub case_id: String,
    pub inputs: Vec<TensorFixtureV1>,
    pub expected_outputs: Vec<ExpectedTensorV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskCompletionParityFixtureV1 {
    pub schema_version: String,
    pub model_id: String,
    pub absolute_tolerance: f32,
    pub cases: Vec<ParityCaseV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParityReportV1 {
    pub model_id: String,
    pub cases: usize,
    pub compared_values: usize,
    pub maximum_absolute_error: f32,
    pub absolute_tolerance: f32,
}

pub struct TaskCompletionOnnxRuntime {
    manifest: TaskCompletionModelManifestV1,
    session: Session,
    tokenizer: Option<tokenizers::Tokenizer>,
}

impl TaskCompletionOnnxRuntime {
    pub fn load(artifact_dir: &Path) -> Result<Self> {
        let (manifest, report) = verify_artifact(artifact_dir)?;
        let session = Session::builder()?.commit_from_file(report.model_path)?;
        validate_session_contract(&session, &manifest)?;
        let tokenizer = report
            .tokenizer_path
            .map(tokenizers::Tokenizer::from_file)
            .transpose()
            .map_err(|error| invalid(format!("tokenizer artifact is invalid: {error}")))?;
        Ok(Self {
            manifest,
            session,
            tokenizer,
        })
    }

    pub fn manifest(&self) -> &TaskCompletionModelManifestV1 {
        &self.manifest
    }

    /// Verify that the immutable evaluator release names exactly the artifacts
    /// and schemas loaded by this runtime. A local model may never be selected
    /// merely because it happens to be present on disk.
    pub fn bind_to_release(&self, release: &EvaluatorReleaseSpecV1) -> Result<()> {
        validate_release_binding(&self.manifest, release)
    }

    /// Count with the tokenizer shipped and hash-verified alongside the model.
    /// Compact projections use this counter instead of bytes or whitespace.
    pub fn count_tokens(&self, text: &str) -> Result<u32> {
        let tokenizer = self
            .tokenizer
            .as_ref()
            .ok_or_else(|| invalid("the local classifier artifact has no tokenizer"))?;
        let encoding = tokenizer
            .encode(text, false)
            .map_err(|error| invalid(format!("tokenization failed: {error}")))?;
        u32::try_from(encoding.len()).map_err(|_| invalid("token count exceeds u32"))
    }

    /// Projected structured evidence is the stable boundary between trace
    /// extraction and the learned model. Feature order comes exclusively from
    /// the versioned contract, never from a hand-maintained Rust array.
    pub fn decide_projection(
        &mut self,
        projection: &CompactTaskCompletionProjectionV1,
    ) -> Result<TaskCompletionDecisionV1> {
        let input = structured_feature_input(projection)?;
        self.decide(&[input])
    }

    pub fn infer(&mut self, inputs: &[TensorFixtureV1]) -> Result<Vec<ExpectedTensorV1>> {
        validate_inputs(inputs, &self.manifest.inputs)?;
        let session_inputs = inputs
            .iter()
            .map(tensor_input)
            .collect::<Result<Vec<_>>>()?;
        let outputs = self.session.run(session_inputs)?;
        self.manifest
            .outputs
            .iter()
            .map(|spec| {
                if spec.element_type != TensorElementTypeV1::F32 {
                    return Err(invalid(format!("output {} must use f32 logits", spec.name)));
                }
                let value = outputs
                    .get(&spec.name)
                    .ok_or_else(|| invalid(format!("model omitted output {}", spec.name)))?;
                let (shape, values) = value.try_extract_tensor::<f32>()?;
                let shape = shape
                    .iter()
                    .map(|dimension| {
                        usize::try_from(*dimension).map_err(|_| {
                            invalid(format!("output {} has a negative dimension", spec.name))
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                validate_shape(&shape, &spec.dimensions, &spec.name)?;
                if !values.iter().all(|value| value.is_finite()) {
                    return Err(invalid(format!(
                        "model output {} contains non-finite values",
                        spec.name
                    )));
                }
                Ok(ExpectedTensorV1 {
                    name: spec.name.clone(),
                    shape,
                    f32_values: values.to_vec(),
                })
            })
            .collect()
    }

    pub fn decide(&mut self, inputs: &[TensorFixtureV1]) -> Result<TaskCompletionDecisionV1> {
        let outputs = self.infer(inputs)?;
        calibrated_decision(&self.manifest, &outputs)
    }

    pub fn run_parity(
        &mut self,
        fixture: &TaskCompletionParityFixtureV1,
    ) -> Result<ParityReportV1> {
        validate_parity_fixture(fixture, &self.manifest)?;
        let mut compared_values = 0_usize;
        let mut maximum_absolute_error = 0.0_f32;
        for case in &fixture.cases {
            let outputs = self.infer(&case.inputs)?;
            for expected in &case.expected_outputs {
                let actual = outputs
                    .iter()
                    .find(|output| output.name == expected.name)
                    .ok_or_else(|| {
                        invalid(format!(
                            "parity case {} omitted output {}",
                            case.case_id, expected.name
                        ))
                    })?;
                if actual.shape != expected.shape
                    || actual.f32_values.len() != expected.f32_values.len()
                {
                    return Err(invalid(format!(
                        "parity case {} output {} shape differs",
                        case.case_id, expected.name
                    )));
                }
                for (actual, expected) in actual.f32_values.iter().zip(&expected.f32_values) {
                    let error = (actual - expected).abs();
                    if !error.is_finite() {
                        return Err(invalid(format!(
                            "parity case {} produced a non-finite error",
                            case.case_id
                        )));
                    }
                    maximum_absolute_error = maximum_absolute_error.max(error);
                    compared_values += 1;
                }
            }
        }
        if maximum_absolute_error > fixture.absolute_tolerance {
            return Err(invalid(format!(
                "maximum parity error {maximum_absolute_error} exceeds tolerance {}",
                fixture.absolute_tolerance
            )));
        }
        Ok(ParityReportV1 {
            model_id: fixture.model_id.clone(),
            cases: fixture.cases.len(),
            compared_values,
            maximum_absolute_error,
            absolute_tolerance: fixture.absolute_tolerance,
        })
    }
}

pub fn structured_feature_input(
    projection: &CompactTaskCompletionProjectionV1,
) -> Result<TensorFixtureV1> {
    let record = TaskCompletionEvidenceFeatureRecordV1::from_projection(projection)
        .map_err(|error| invalid(format!("structured evidence is invalid: {error}")))?;
    let values = record
        .feature_values
        .into_iter()
        .map(|value| {
            let value = value as f32;
            value
                .is_finite()
                .then_some(value)
                .ok_or_else(|| invalid("structured evidence cannot be represented as f32"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(TensorFixtureV1 {
        name: TASK_COMPLETION_STRUCTURED_INPUT_NAME.into(),
        element_type: TensorElementTypeV1::F32,
        shape: vec![1, values.len()],
        f32_values: values,
        i64_values: Vec::new(),
    })
}

pub fn validate_release_binding(
    manifest: &TaskCompletionModelManifestV1,
    release: &EvaluatorReleaseSpecV1,
) -> Result<()> {
    validate_manifest(manifest)?;
    release
        .validate()
        .map_err(|error| invalid(format!("evaluator release is invalid: {error}")))?;
    if release.task_kind != LearnedTaskKind::TaskCompletion {
        return Err(invalid("local artifact requires a task-completion release"));
    }
    let EvaluationImplementationV1::LocalClassifier {
        model_artifact_id,
        tokenizer_artifact_id,
        feature_schema_id,
        runtime_version,
    } = &release.implementation
    else {
        return Err(invalid("evaluator release is not a local classifier"));
    };
    let tokenizer = manifest
        .tokenizer_file
        .as_ref()
        .ok_or_else(|| invalid("local classifier manifest must name a tokenizer artifact"))?;
    for (actual, expected, field) in [
        (
            model_artifact_id.as_str(),
            manifest.model_file.sha256.as_str(),
            "model artifact",
        ),
        (
            tokenizer_artifact_id.as_str(),
            tokenizer.sha256.as_str(),
            "tokenizer artifact",
        ),
        (
            feature_schema_id.as_str(),
            manifest.lineage.feature_set_version.as_str(),
            "feature schema",
        ),
        (
            runtime_version.as_str(),
            TASK_COMPLETION_ONNX_RUNTIME_VERSION,
            "runtime version",
        ),
    ] {
        if actual != expected {
            return Err(invalid(format!(
                "evaluator {field} does not match the verified local artifact: expected \
                 {expected}, got {actual}"
            )));
        }
    }
    Ok(())
}

pub fn calibrated_decision(
    manifest: &TaskCompletionModelManifestV1,
    outputs: &[ExpectedTensorV1],
) -> Result<TaskCompletionDecisionV1> {
    validate_manifest(manifest)?;
    validate_outputs(outputs, &manifest.outputs)?;
    let decision = &manifest.decision_output;
    let logits = outputs
        .iter()
        .find(|output| output.name == decision.output_name)
        .ok_or_else(|| invalid("model output does not contain the configured decision logits"))?;
    if logits.shape.first() != Some(&1) {
        return Err(invalid(
            "calibrated decisions require exactly one trace per inference",
        ));
    }
    let incomplete = logits
        .f32_values
        .get(decision.incomplete_logit_index)
        .copied()
        .ok_or_else(|| invalid("incomplete logit index is outside the model output"))?;
    let complete = logits
        .f32_values
        .get(decision.complete_logit_index)
        .copied()
        .ok_or_else(|| invalid("complete logit index is outside the model output"))?;
    if !incomplete.is_finite() || !complete.is_finite() {
        return Err(invalid("decision logits must be finite"));
    }
    let raw_logit_margin = complete - incomplete;
    let scaled_margin = raw_logit_margin / manifest.calibration.temperature;
    let calibrated_probability_complete = stable_sigmoid(scaled_margin);
    let label = if calibrated_probability_complete >= manifest.calibration.threshold_complete {
        TaskCompletionLabelV1::Complete
    } else {
        TaskCompletionLabelV1::Incomplete
    };
    Ok(TaskCompletionDecisionV1 {
        model_id: manifest.model_id.clone(),
        label,
        raw_logit_margin,
        calibrated_probability_complete,
        threshold_complete: manifest.calibration.threshold_complete,
        calibration_version: manifest.calibration.version.clone(),
    })
}

pub fn load_manifest(artifact_dir: &Path) -> Result<TaskCompletionModelManifestV1> {
    let manifest_path = resolved_artifact_path(artifact_dir, Path::new("manifest.json"))?;
    let manifest: TaskCompletionModelManifestV1 =
        serde_json::from_slice(&fs::read(&manifest_path)?)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

pub fn load_parity_fixture(
    artifact_dir: &Path,
    manifest: &TaskCompletionModelManifestV1,
) -> Result<TaskCompletionParityFixtureV1> {
    let path = verified_file(artifact_dir, &manifest.parity_file)?;
    let fixture: TaskCompletionParityFixtureV1 = serde_json::from_slice(&fs::read(path)?)?;
    validate_parity_fixture(&fixture, manifest)?;
    Ok(fixture)
}

pub fn verify_artifact(
    artifact_dir: &Path,
) -> Result<(TaskCompletionModelManifestV1, ArtifactVerificationReportV1)> {
    if !artifact_dir.is_dir() {
        return Err(invalid("artifact directory does not exist"));
    }
    let manifest = load_manifest(artifact_dir)?;
    let model_path = verified_file(artifact_dir, &manifest.model_file)?;
    let parity_path = verified_file(artifact_dir, &manifest.parity_file)?;
    let tokenizer_path = manifest
        .tokenizer_file
        .as_ref()
        .map(|file| verified_file(artifact_dir, file))
        .transpose()?;
    let verified_files = 2 + usize::from(tokenizer_path.is_some());
    let report = ArtifactVerificationReportV1 {
        model_id: manifest.model_id.clone(),
        model_path,
        parity_path,
        tokenizer_path,
        verified_files,
    };
    Ok((manifest, report))
}

fn validate_manifest(manifest: &TaskCompletionModelManifestV1) -> Result<()> {
    if manifest.schema_version != TASK_COMPLETION_MODEL_MANIFEST_SCHEMA_VERSION {
        return Err(invalid("unsupported model manifest schema"));
    }
    require_non_empty(&manifest.model_id, "model_id")?;
    if manifest.model_file.path == manifest.parity_file.path {
        return Err(invalid("model and parity files must be distinct"));
    }
    if !manifest.model_file.path.ends_with(".onnx") {
        return Err(invalid("model file must use the .onnx extension"));
    }
    validate_file_record(&manifest.model_file)?;
    validate_file_record(&manifest.parity_file)?;
    if !manifest.parity_absolute_tolerance.is_finite()
        || manifest.parity_absolute_tolerance <= 0.0
        || manifest.parity_absolute_tolerance > 0.01
    {
        return Err(invalid(
            "manifest parity tolerance must be within (0, 0.01]",
        ));
    }
    if let Some(file) = &manifest.tokenizer_file {
        validate_file_record(file)?;
    }
    validate_tensor_specs(&manifest.inputs, "input")?;
    validate_tensor_specs(&manifest.outputs, "output")?;
    if manifest
        .outputs
        .iter()
        .any(|output| output.element_type != TensorElementTypeV1::F32)
    {
        return Err(invalid("all decision outputs must use f32"));
    }
    validate_decision_output(&manifest.decision_output, &manifest.outputs)?;
    validate_calibration(&manifest.calibration)?;
    if manifest.calibration.version != manifest.lineage.calibration_version {
        return Err(invalid(
            "calibration version differs from the model lineage",
        ));
    }
    validate_lineage(&manifest.lineage)?;
    Ok(())
}

fn validate_decision_output(decision: &DecisionOutputV1, outputs: &[TensorSpecV1]) -> Result<()> {
    require_non_empty(&decision.output_name, "decision output name")?;
    if decision.incomplete_logit_index == decision.complete_logit_index {
        return Err(invalid(
            "completion verbalizer logits must use distinct indices",
        ));
    }
    let output = outputs
        .iter()
        .find(|output| output.name == decision.output_name)
        .ok_or_else(|| invalid("decision output is not declared by the model"))?;
    let class_count = output
        .dimensions
        .last()
        .copied()
        .flatten()
        .ok_or_else(|| invalid("decision output must declare a fixed class dimension"))?;
    if decision.incomplete_logit_index >= class_count
        || decision.complete_logit_index >= class_count
    {
        return Err(invalid(
            "completion verbalizer index exceeds the declared class dimension",
        ));
    }
    Ok(())
}

fn validate_lineage(lineage: &ModelLineageV1) -> Result<()> {
    for (value, field) in [
        (&lineage.base_model_id, "base_model_id"),
        (&lineage.base_model_revision, "base_model_revision"),
        (&lineage.projector_version, "projector_version"),
        (&lineage.dataset_version, "dataset_version"),
        (&lineage.training_version, "training_version"),
        (&lineage.calibration_version, "calibration_version"),
    ] {
        require_non_empty(value, field)?;
    }
    if lineage.projector_schema_version != COMPACT_TASK_COMPLETION_PROJECTION_SCHEMA_VERSION {
        return Err(invalid("manifest uses an unsupported projector schema"));
    }
    if lineage.training_record_schema_version != TASK_COMPLETION_TRAINING_RECORD_SCHEMA_VERSION {
        return Err(invalid(
            "manifest uses an unsupported training-record schema",
        ));
    }
    if lineage.feature_set_version != TASK_COMPLETION_STRUCTURED_FEATURE_SET_VERSION {
        return Err(invalid(
            "manifest uses an unsupported structured feature set",
        ));
    }
    require_sha256(&lineage.dataset_sha256, "dataset_sha256")
}

fn validate_calibration(calibration: &CalibrationManifestV1) -> Result<()> {
    require_non_empty(&calibration.version, "calibration version")?;
    if !calibration.threshold_complete.is_finite()
        || !(0.0..=1.0).contains(&calibration.threshold_complete)
    {
        return Err(invalid("calibration threshold must be within [0, 1]"));
    }
    if !calibration.temperature.is_finite() || calibration.temperature <= 0.0 {
        return Err(invalid("calibration temperature must be positive"));
    }
    Ok(())
}

fn validate_tensor_specs(specs: &[TensorSpecV1], kind: &str) -> Result<()> {
    if specs.is_empty() {
        return Err(invalid(format!("manifest requires at least one {kind}")));
    }
    let mut names = BTreeSet::new();
    for spec in specs {
        require_non_empty(&spec.name, &format!("{kind} tensor name"))?;
        if !names.insert(spec.name.as_str()) {
            return Err(invalid(format!("duplicate {kind} tensor {}", spec.name)));
        }
        if spec.dimensions.is_empty()
            || spec
                .dimensions
                .iter()
                .any(|dimension| dimension == &Some(0))
        {
            return Err(invalid(format!(
                "{kind} tensor {} has invalid dimensions",
                spec.name
            )));
        }
    }
    Ok(())
}

fn validate_session_contract(
    session: &Session,
    manifest: &TaskCompletionModelManifestV1,
) -> Result<()> {
    let session_inputs = session
        .inputs()
        .iter()
        .map(|input| input.name())
        .collect::<BTreeSet<_>>();
    let manifest_inputs = manifest
        .inputs
        .iter()
        .map(|input| input.name.as_str())
        .collect::<BTreeSet<_>>();
    if session_inputs != manifest_inputs {
        return Err(invalid("ONNX input names do not match the manifest"));
    }
    let session_outputs = session
        .outputs()
        .iter()
        .map(|output| output.name())
        .collect::<BTreeSet<_>>();
    let manifest_outputs = manifest
        .outputs
        .iter()
        .map(|output| output.name.as_str())
        .collect::<BTreeSet<_>>();
    if session_outputs != manifest_outputs {
        return Err(invalid("ONNX output names do not match the manifest"));
    }
    Ok(())
}

fn validate_inputs(inputs: &[TensorFixtureV1], specs: &[TensorSpecV1]) -> Result<()> {
    if inputs.len() != specs.len() {
        return Err(invalid("input tensor count does not match the manifest"));
    }
    for spec in specs {
        let input = inputs
            .iter()
            .find(|input| input.name == spec.name)
            .ok_or_else(|| invalid(format!("missing input tensor {}", spec.name)))?;
        if input.element_type != spec.element_type {
            return Err(invalid(format!(
                "input {} has the wrong element type",
                spec.name
            )));
        }
        validate_shape(&input.shape, &spec.dimensions, &spec.name)?;
        let elements = element_count(&input.shape)?;
        match input.element_type {
            TensorElementTypeV1::F32 => {
                if input.f32_values.len() != elements || !input.i64_values.is_empty() {
                    return Err(invalid(format!("input {} has invalid f32 data", spec.name)));
                }
                if !input.f32_values.iter().all(|value| value.is_finite()) {
                    return Err(invalid(format!(
                        "input {} contains non-finite values",
                        spec.name
                    )));
                }
            }
            TensorElementTypeV1::I64 => {
                if input.i64_values.len() != elements || !input.f32_values.is_empty() {
                    return Err(invalid(format!("input {} has invalid i64 data", spec.name)));
                }
            }
        }
    }
    Ok(())
}

fn validate_outputs(outputs: &[ExpectedTensorV1], specs: &[TensorSpecV1]) -> Result<()> {
    if outputs.len() != specs.len() {
        return Err(invalid("output tensor count does not match the manifest"));
    }
    for spec in specs {
        let output = outputs
            .iter()
            .find(|output| output.name == spec.name)
            .ok_or_else(|| invalid(format!("missing output tensor {}", spec.name)))?;
        validate_shape(&output.shape, &spec.dimensions, &spec.name)?;
        if output.f32_values.len() != element_count(&output.shape)?
            || !output.f32_values.iter().all(|value| value.is_finite())
        {
            return Err(invalid(format!(
                "output {} contains invalid f32 data",
                spec.name
            )));
        }
    }
    Ok(())
}

fn tensor_input(
    input: &TensorFixtureV1,
) -> Result<(Cow<'static, str>, SessionInputValue<'static>)> {
    let shape = input
        .shape
        .iter()
        .map(|dimension| {
            i64::try_from(*dimension).map_err(|_| invalid("tensor dimension exceeds i64"))
        })
        .collect::<Result<Vec<_>>>()?;
    let value = match input.element_type {
        TensorElementTypeV1::F32 => Tensor::from_array((shape, input.f32_values.clone()))?.into(),
        TensorElementTypeV1::I64 => Tensor::from_array((shape, input.i64_values.clone()))?.into(),
    };
    Ok((Cow::Owned(input.name.clone()), value))
}

fn validate_parity_fixture(
    fixture: &TaskCompletionParityFixtureV1,
    manifest: &TaskCompletionModelManifestV1,
) -> Result<()> {
    if fixture.schema_version != TASK_COMPLETION_PARITY_FIXTURE_SCHEMA_VERSION {
        return Err(invalid("unsupported parity fixture schema"));
    }
    if fixture.model_id != manifest.model_id {
        return Err(invalid("parity fixture model_id differs from the manifest"));
    }
    if !fixture.absolute_tolerance.is_finite() || fixture.absolute_tolerance <= 0.0 {
        return Err(invalid("parity tolerance must be positive"));
    }
    if fixture.absolute_tolerance != manifest.parity_absolute_tolerance {
        return Err(invalid(
            "parity fixture tolerance differs from the model manifest",
        ));
    }
    if fixture.cases.is_empty() {
        return Err(invalid("parity fixture requires at least one case"));
    }
    let mut ids = BTreeSet::new();
    for case in &fixture.cases {
        require_non_empty(&case.case_id, "parity case_id")?;
        if !ids.insert(case.case_id.as_str()) {
            return Err(invalid(format!("duplicate parity case {}", case.case_id)));
        }
        validate_inputs(&case.inputs, &manifest.inputs)?;
        if case.expected_outputs.len() != manifest.outputs.len() {
            return Err(invalid(format!(
                "parity case {} output count differs from the manifest",
                case.case_id
            )));
        }
        for spec in &manifest.outputs {
            let output = case
                .expected_outputs
                .iter()
                .find(|output| output.name == spec.name)
                .ok_or_else(|| {
                    invalid(format!(
                        "parity case {} lacks output {}",
                        case.case_id, spec.name
                    ))
                })?;
            validate_shape(&output.shape, &spec.dimensions, &spec.name)?;
            if output.f32_values.len() != element_count(&output.shape)?
                || !output.f32_values.iter().all(|value| value.is_finite())
            {
                return Err(invalid(format!(
                    "parity case {} output {} has invalid data",
                    case.case_id, spec.name
                )));
            }
        }
    }
    Ok(())
}

fn validate_shape(shape: &[usize], expected: &[Option<usize>], name: &str) -> Result<()> {
    if shape.len() != expected.len()
        || shape.iter().zip(expected).any(|(actual, expected)| {
            *actual == 0 || expected.is_some_and(|value| value != *actual)
        })
    {
        return Err(invalid(format!(
            "tensor {name} shape does not match the manifest"
        )));
    }
    Ok(())
}

fn element_count(shape: &[usize]) -> Result<usize> {
    shape.iter().try_fold(1_usize, |product, dimension| {
        product
            .checked_mul(*dimension)
            .ok_or_else(|| invalid("tensor element count overflowed"))
    })
}

fn validate_file_record(file: &ArtifactFileV1) -> Result<()> {
    safe_relative_path(&file.path)?;
    require_sha256(&file.sha256, "artifact file sha256")?;
    if file.size_bytes == 0 {
        return Err(invalid("artifact file size must be greater than zero"));
    }
    Ok(())
}

fn verified_file(artifact_dir: &Path, file: &ArtifactFileV1) -> Result<PathBuf> {
    validate_file_record(file)?;
    let path = resolved_artifact_path(artifact_dir, &safe_relative_path(&file.path)?)?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() || metadata.len() != file.size_bytes {
        return Err(invalid(format!(
            "artifact file {} has the wrong size",
            file.path
        )));
    }
    let digest = sha256_file(&path)?;
    if digest != file.sha256 {
        return Err(invalid(format!(
            "artifact file {} has the wrong hash",
            file.path
        )));
    }
    Ok(path)
}

fn resolved_artifact_path(artifact_dir: &Path, relative: &Path) -> Result<PathBuf> {
    let root = fs::canonicalize(artifact_dir)?;
    let path = fs::canonicalize(artifact_dir.join(relative))?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err(invalid("artifact file resolves outside its directory"));
    }
    Ok(path)
}

fn safe_relative_path(value: &str) -> Result<PathBuf> {
    require_non_empty(value, "artifact path")?;
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid(
            "artifact paths must stay inside the artifact directory",
        ));
    }
    Ok(path.to_path_buf())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let bytes = file.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        digest.update(&buffer[..bytes]);
    }
    Ok(format!("sha256:{}", hex::encode(digest.finalize())))
}

fn require_sha256(value: &str, field: &str) -> Result<()> {
    let digest = value.strip_prefix("sha256:").unwrap_or_default();
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(invalid(format!(
            "{field} must use sha256:<64 lowercase hex>"
        )));
    }
    Ok(())
}

fn stable_sigmoid(value: f32) -> f32 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exponential = value.exp();
        exponential / (1.0 + exponential)
    }
}

fn require_non_empty(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(invalid(format!("{field} must not be empty")));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> RuntimeError {
    RuntimeError::InvalidArtifact(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn manifest() -> TaskCompletionModelManifestV1 {
        TaskCompletionModelManifestV1 {
            schema_version: TASK_COMPLETION_MODEL_MANIFEST_SCHEMA_VERSION.into(),
            model_id: "perseval-task-completion@test".into(),
            model_file: ArtifactFileV1 {
                path: "model.onnx".into(),
                sha256: digest('1'),
                size_bytes: 10,
            },
            tokenizer_file: None,
            parity_file: ArtifactFileV1 {
                path: "parity.json".into(),
                sha256: digest('2'),
                size_bytes: 10,
            },
            parity_absolute_tolerance: 1e-4,
            inputs: vec![TensorSpecV1 {
                name: "structured_features".into(),
                element_type: TensorElementTypeV1::F32,
                dimensions: vec![None, Some(39)],
            }],
            outputs: vec![TensorSpecV1 {
                name: "logits".into(),
                element_type: TensorElementTypeV1::F32,
                dimensions: vec![None, Some(2)],
            }],
            decision_output: DecisionOutputV1 {
                output_name: "logits".into(),
                incomplete_logit_index: 0,
                complete_logit_index: 1,
            },
            calibration: CalibrationManifestV1 {
                version: "calibration-v1".into(),
                threshold_complete: 0.5,
                temperature: 1.0,
            },
            lineage: ModelLineageV1 {
                base_model_id: "modernbert-base".into(),
                base_model_revision: "revision".into(),
                projector_schema_version: COMPACT_TASK_COMPLETION_PROJECTION_SCHEMA_VERSION.into(),
                projector_version: "projector-v1".into(),
                training_record_schema_version: TASK_COMPLETION_TRAINING_RECORD_SCHEMA_VERSION
                    .into(),
                feature_set_version: TASK_COMPLETION_STRUCTURED_FEATURE_SET_VERSION.into(),
                dataset_version: "dataset-v1".into(),
                dataset_sha256: digest('3'),
                training_version: "training-v1".into(),
                calibration_version: "calibration-v1".into(),
            },
        }
    }

    #[test]
    fn manifest_rejects_path_escape_and_invalid_calibration() {
        let mut manifest = manifest();
        validate_manifest(&manifest).unwrap();

        manifest.model_file.path = "../model.onnx".into();
        assert!(validate_manifest(&manifest).is_err());
        manifest.model_file.path = "model.onnx".into();
        manifest.calibration.temperature = 0.0;
        assert!(validate_manifest(&manifest).is_err());
        manifest.calibration.temperature = 1.0;
        manifest.decision_output.complete_logit_index = 2;
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn local_release_must_bind_every_verified_artifact_and_schema() {
        let mut manifest = manifest();
        manifest.tokenizer_file = Some(ArtifactFileV1 {
            path: "tokenizer.json".into(),
            sha256: digest('4'),
            size_bytes: 10,
        });
        let mut release = traceeval_contracts::EvaluatorReleaseSpecV1 {
            schema_version: traceeval_contracts::EVALUATOR_RELEASE_SCHEMA_VERSION.into(),
            name: "local task completion".into(),
            task_kind: traceeval_contracts::LearnedTaskKind::TaskCompletion,
            target_kind: traceeval_contracts::EvaluationTargetKind::TraceRevision,
            implementation: traceeval_contracts::EvaluationImplementationV1::LocalClassifier {
                model_artifact_id: manifest.model_file.sha256.clone(),
                tokenizer_artifact_id: manifest.tokenizer_file.as_ref().unwrap().sha256.clone(),
                feature_schema_id: manifest.lineage.feature_set_version.clone(),
                runtime_version: TASK_COMPLETION_ONNX_RUNTIME_VERSION.into(),
            },
            projection_release_id: digest('5'),
            context_projection_release_id: digest('6'),
            applicable_taxonomy_release_id: None,
            applicable_taxonomy_node_ids: Default::default(),
            input_bounds: traceeval_contracts::EvaluationInputBoundsV1 {
                max_subjects: 1,
                max_evidence_items: 64,
                max_input_bytes: 100_000,
                max_output_bytes: 10_000,
            },
            evidence_schema_version: "traceeval.evidence.v1".into(),
            abstention_policy: serde_json::json!({"missing_evidence": "abstain"}),
            code_artifact_hash: digest('7'),
        };

        validate_release_binding(&manifest, &release).unwrap();
        if let traceeval_contracts::EvaluationImplementationV1::LocalClassifier {
            runtime_version,
            ..
        } = &mut release.implementation
        {
            *runtime_version = "wrong-runtime".into();
        }
        let error = validate_release_binding(&manifest, &release)
            .expect_err("a mismatched runtime must fail release binding")
            .to_string();
        assert!(error.contains(TASK_COMPLETION_ONNX_RUNTIME_VERSION));
        assert!(error.contains("wrong-runtime"));
    }

    #[test]
    fn input_validation_is_typed_and_shape_bound() {
        let manifest = manifest();
        let mut input = TensorFixtureV1 {
            name: "structured_features".into(),
            element_type: TensorElementTypeV1::F32,
            shape: vec![1, 39],
            f32_values: vec![0.0; 39],
            i64_values: Vec::new(),
        };
        validate_inputs(&[input.clone()], &manifest.inputs).unwrap();
        input.shape = vec![1, 38];
        input.f32_values.pop();
        assert!(validate_inputs(&[input], &manifest.inputs).is_err());
    }

    #[test]
    fn decision_uses_versioned_temperature_and_threshold() {
        let mut manifest = manifest();
        manifest.calibration.threshold_complete = 0.75;
        manifest.calibration.temperature = 2.0;
        let outputs = vec![ExpectedTensorV1 {
            name: "logits".into(),
            shape: vec![1, 2],
            f32_values: vec![0.0, 2.0],
        }];

        let decision = calibrated_decision(&manifest, &outputs).unwrap();

        assert_eq!(decision.label, TaskCompletionLabelV1::Incomplete);
        assert!((decision.calibrated_probability_complete - 0.731_058_6).abs() < 1e-6);
        assert_eq!(decision.calibration_version, "calibration-v1");
    }

    #[test]
    fn decision_rejects_missing_or_non_finite_logits() {
        let manifest = manifest();
        assert!(calibrated_decision(&manifest, &[]).is_err());
        let outputs = vec![ExpectedTensorV1 {
            name: "logits".into(),
            shape: vec![1, 2],
            f32_values: vec![0.0, f32::NAN],
        }];
        assert!(calibrated_decision(&manifest, &outputs).is_err());

        let outputs = vec![ExpectedTensorV1 {
            name: "logits".into(),
            shape: vec![2, 2],
            f32_values: vec![0.0; 4],
        }];
        assert!(calibrated_decision(&manifest, &outputs).is_err());
    }

    #[test]
    fn parity_fixture_tolerance_must_match_the_manifest() {
        let manifest = manifest();
        let fixture = TaskCompletionParityFixtureV1 {
            schema_version: TASK_COMPLETION_PARITY_FIXTURE_SCHEMA_VERSION.into(),
            model_id: manifest.model_id.clone(),
            absolute_tolerance: 1e-3,
            cases: vec![ParityCaseV1 {
                case_id: "case-1".into(),
                inputs: vec![TensorFixtureV1 {
                    name: "structured_features".into(),
                    element_type: TensorElementTypeV1::F32,
                    shape: vec![1, 39],
                    f32_values: vec![0.0; 39],
                    i64_values: Vec::new(),
                }],
                expected_outputs: vec![ExpectedTensorV1 {
                    name: "logits".into(),
                    shape: vec![1, 2],
                    f32_values: vec![0.0, 0.0],
                }],
            }],
        };

        assert!(validate_parity_fixture(&fixture, &manifest).is_err());
    }

    #[test]
    fn verified_artifact_rejects_hash_mismatch() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("model.onnx"), b"not-an-onnx").unwrap();
        fs::write(directory.path().join("parity.json"), b"{}").unwrap();
        let mut manifest = manifest();
        manifest.model_file.size_bytes = 11;
        manifest.parity_file.size_bytes = 2;
        fs::write(
            directory.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(verify_artifact(directory.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn artifact_resolution_rejects_symlinks_outside_the_directory() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), directory.path().join("model.onnx")).unwrap();

        assert!(resolved_artifact_path(directory.path(), Path::new("model.onnx")).is_err());
    }
}
