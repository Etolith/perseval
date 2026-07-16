use perseval_service::{FailureFiltersV1, QueryScopeCriteriaV1, QueryScopeV1, RunFiltersV1};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PageInput {
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ProjectSelector {
    Project { project_id: String },
    AllProjects,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScopeInput {
    pub project: ProjectSelector,
    pub environment: Option<String>,
    pub build_id: Option<String>,
    pub session_id: Option<String>,
    pub started_at_or_after_unix_nano: Option<String>,
    pub started_before_unix_nano: Option<String>,
}

impl ScopeInput {
    pub(crate) fn query_scope(&self, allow_all: bool) -> Result<QueryScopeV1, String> {
        let project_id = match &self.project {
            ProjectSelector::Project { project_id } if !project_id.trim().is_empty() => {
                Some(project_id.clone())
            }
            ProjectSelector::Project { .. } => return Err("project_id cannot be empty".into()),
            ProjectSelector::AllProjects if allow_all => None,
            ProjectSelector::AllProjects => {
                return Err("this tool requires one concrete project".into());
            }
        };
        let started_after_unix_nano = parse_decimal(
            "started_at_or_after_unix_nano",
            self.started_at_or_after_unix_nano.as_deref(),
        )?;
        let started_before_unix_nano = parse_decimal(
            "started_before_unix_nano",
            self.started_before_unix_nano.as_deref(),
        )?;
        if started_after_unix_nano
            .zip(started_before_unix_nano)
            .is_some_and(|(start, end)| start >= end)
        {
            return Err("scope time bounds must form a non-empty half-open interval".into());
        }
        Ok(QueryScopeV1::new(QueryScopeCriteriaV1 {
            project_id,
            environment: clean_optional(&self.environment),
            build_id: clean_optional(&self.build_id),
            session_id: clean_optional(&self.session_id),
            service_name: None,
            started_after_unix_nano,
            started_before_unix_nano,
        }))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionSort {
    #[default]
    Newest,
    Oldest,
    MostRuns,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListSessionsInput {
    pub scope: ScopeInput,
    pub sort: Option<SessionSort>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunSort {
    #[default]
    Newest,
    Oldest,
    MostSpans,
    MostFindings,
}

impl From<RunSort> for perseval_service::RunOrderV1 {
    fn from(value: RunSort) -> Self {
        match value {
            RunSort::Newest => Self::Newest,
            RunSort::Oldest => Self::Oldest,
            RunSort::MostSpans => Self::MostSpans,
            RunSort::MostFindings => Self::MostFindings,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LifecycleInput {
    Live,
    Quiescent,
    Finalized,
    Reopened,
}

impl From<LifecycleInput> for perseval_service::TraceLifecycle {
    fn from(value: LifecycleInput) -> Self {
        match value {
            LifecycleInput::Live => Self::Live,
            LifecycleInput::Quiescent => Self::Quiescent,
            LifecycleInput::Finalized => Self::Finalized,
            LifecycleInput::Reopened => Self::Reopened,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AnalysisStatusInput {
    NotReady,
    Pending,
    Analyzing,
    Ready,
    Reanalyzing,
    Failed,
}

impl AnalysisStatusInput {
    pub(crate) fn matches(self, value: perseval_service::AnalysisStatus) -> bool {
        matches!(
            (self, value),
            (Self::NotReady, perseval_service::AnalysisStatus::NotReady)
                | (Self::Pending, perseval_service::AnalysisStatus::Pending)
                | (Self::Analyzing, perseval_service::AnalysisStatus::Analyzing)
                | (Self::Ready, perseval_service::AnalysisStatus::Ready)
                | (
                    Self::Reanalyzing,
                    perseval_service::AnalysisStatus::Reanalyzing
                )
                | (Self::Failed, perseval_service::AnalysisStatus::Failed)
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IdentityQualityInput {
    Explicit,
    Inferred,
    Unknown,
}

impl From<IdentityQualityInput> for perseval_service::IdentityQualityV1 {
    fn from(value: IdentityQualityInput) -> Self {
        match value {
            IdentityQualityInput::Explicit => Self::Explicit,
            IdentityQualityInput::Inferred => Self::Inferred,
            IdentityQualityInput::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListRunsInput {
    pub scope: ScopeInput,
    #[serde(default)]
    pub lifecycle: Vec<LifecycleInput>,
    #[serde(default)]
    pub analysis_status: Vec<AnalysisStatusInput>,
    #[serde(default)]
    pub identity_quality: Vec<IdentityQualityInput>,
    pub search: Option<String>,
    pub sort: Option<RunSort>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

impl ListRunsInput {
    pub(crate) fn filters(&self) -> Result<RunFiltersV1, String> {
        ensure_at_most_one("lifecycle", &self.lifecycle)?;
        ensure_at_most_one("identity_quality", &self.identity_quality)?;
        if self.analysis_status.len() > 1 {
            return Err("analysis_status currently accepts at most one value".into());
        }
        Ok(RunFiltersV1 {
            scope: self.scope.query_scope(true)?,
            lifecycle: self.lifecycle.first().copied().map(Into::into),
            identity_quality: self.identity_quality.first().copied().map(Into::into),
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GroupSort {
    #[default]
    Priority,
    MostFrequent,
    Newest,
    LargestIncrease,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SeverityInput {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl From<SeverityInput> for perseval_service::FindingSeverity {
    fn from(value: SeverityInput) -> Self {
        match value {
            SeverityInput::Info => Self::Info,
            SeverityInput::Low => Self::Low,
            SeverityInput::Medium => Self::Medium,
            SeverityInput::High => Self::High,
            SeverityInput::Critical => Self::Critical,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecoveryInput {
    Recovered,
    Unrecovered,
    Unknown,
}

impl From<RecoveryInput> for perseval_service::RecoveryStatus {
    fn from(value: RecoveryInput) -> Self {
        match value {
            RecoveryInput::Recovered => Self::Recovered,
            RecoveryInput::Unrecovered => Self::Unrecovered,
            RecoveryInput::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListFailureGroupsInput {
    pub scope: ScopeInput,
    #[serde(default)]
    pub severity: Vec<SeverityInput>,
    #[serde(default)]
    pub recovery: Vec<RecoveryInput>,
    #[serde(default)]
    pub detector_id: Vec<String>,
    pub search: Option<String>,
    #[serde(default)]
    pub include_fully_dismissed: bool,
    pub sort: Option<GroupSort>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

impl ListFailureGroupsInput {
    pub(crate) fn filters(&self) -> Result<FailureFiltersV1, String> {
        ensure_at_most_one("severity", &self.severity)?;
        ensure_at_most_one("recovery", &self.recovery)?;
        ensure_at_most_one("detector_id", &self.detector_id)?;
        Ok(FailureFiltersV1 {
            scope: self.scope.query_scope(true)?,
            severity: self.severity.first().copied().map(Into::into),
            recovery: self.recovery.first().copied().map(Into::into),
            detector_id: self.detector_id.first().cloned(),
            search: clean_optional(&self.search),
            include_fully_dismissed: self.include_fully_dismissed,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetFailureGroupInput {
    pub scope: ScopeInput,
    pub group_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct InspectFindingInput {
    pub scope: ScopeInput,
    pub group_id: String,
    pub finding_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetEvidenceTraceInput {
    pub scope: ScopeInput,
    pub group_id: String,
    pub finding_id: String,
    pub before: Option<u32>,
    pub after: Option<u32>,
    pub maximum_spans: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetEvalBatchJobInput {
    pub scope: ScopeInput,
    pub job_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetVerificationReportInput {
    pub scope: ScopeInput,
    pub job_id: Option<String>,
    pub report_id: Option<String>,
}

pub(crate) fn input_fingerprint<T: Serialize>(input: &T) -> String {
    let bytes = serde_json::to_vec(input).expect("MCP input is serializable");
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn parse_decimal(name: &str, value: Option<&str>) -> Result<Option<u64>, String> {
    value
        .map(|value| {
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(format!("{name} must be a decimal u64 string"));
            }
            value
                .parse::<u64>()
                .map_err(|_| format!("{name} exceeds the u64 range"))
        })
        .transpose()
}

fn clean_optional(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn ensure_at_most_one<T>(name: &str, values: &[T]) -> Result<(), String> {
    if values.len() > 1 {
        Err(format!("{name} currently accepts at most one value"))
    } else {
        Ok(())
    }
}
