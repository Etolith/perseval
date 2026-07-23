use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use perseval_store::DEFAULT_INLINE_ATTRIBUTE_BYTES;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CONFIG_SCHEMA_VERSION: u32 = 1;
pub const CONFIG_ENV: &str = "PERSEVAL_CONFIG";
pub const WORKSPACE_ENV: &str = "PERSEVAL_WORKSPACE_DIR";
pub const OTLP_ENABLED_ENV: &str = "PERSEVAL_OTLP_ENABLED";
pub const OTLP_BIND_ENV: &str = "PERSEVAL_OTLP_BIND";
pub const REVIEWER_REF_ENV: &str = "PERSEVAL_REVIEWER_REF";
pub const MCP_READ_ENABLED_ENV: &str = "PERSEVAL_MCP_READ_ENABLED";
pub const MCP_COMPUTE_ENABLED_ENV: &str = "PERSEVAL_MCP_COMPUTE_ENABLED";
pub const MCP_WRITE_ENABLED_ENV: &str = "PERSEVAL_MCP_WRITE_ENABLED";
pub const MCP_PAYLOAD_REVEAL_ENABLED_ENV: &str = "PERSEVAL_MCP_PAYLOAD_REVEAL_ENABLED";
pub const OPENAI_ENABLED_ENV: &str = "PERSEVAL_OPENAI_ENABLED";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PersevalConfigV1 {
    pub schema_version: u32,
    pub workspace_id: String,
    pub workspace_dir: PathBuf,
    pub reviewer_ref: String,
    pub otlp: OtlpConfig,
    pub stream: StreamConfig,
    pub lifecycle: LifecycleConfig,
    pub analysis: AnalysisConfig,
    pub assessments: AssessmentConfig,
    pub query: QueryConfig,
    pub blobs: BlobConfig,
    pub mcp: McpConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OtlpConfig {
    pub enabled: bool,
    pub bind_addr: SocketAddr,
    pub source_id: String,
    pub max_wire_bytes: usize,
    pub max_decoded_bytes: usize,
    pub max_spans_per_request: usize,
    pub max_attributes_per_span: usize,
    pub retry_after_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StreamConfig {
    pub queue_batches: usize,
    pub queue_bytes: usize,
    pub microbatch_spans: usize,
    pub microbatch_bytes: usize,
    pub microbatch_wait_ms: u64,
    pub projection_retry_page: usize,
    pub projection_retry_initial_ms: u64,
    pub projection_retry_max_ms: u64,
    pub topology_chunk_rows: usize,
    pub pipeline_metrics_flush_ms: u64,
    pub delta_history: usize,
    pub subscriber_capacity: usize,
    pub ui_max_deltas_per_frame: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LifecycleConfig {
    pub idle_ms: u64,
    pub finalization_grace_ms: u64,
    pub sweep_ms: u64,
    pub shutdown_drain_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnalysisConfig {
    #[serde(alias = "semantic_clustering_enabled")]
    pub feature_similarity_enabled: bool,
    pub cohort_rebuild_debounce_ms: u64,
    pub cohort_job_queue: usize,
    pub cohort_quality_sample_size: usize,
    pub cohort_rebuild_new_percent: u32,
    pub cohort_rebuild_new_cases: usize,
    pub cohort_rebuild_novelty_percent: u32,
    pub cohort_model_history: usize,
    pub cohort_feature_cache_entries: usize,
    pub cohort_maximum_cases: usize,
    pub embedding_dimensions: usize,
    pub maximum_clusters: usize,
    pub minimum_findings: usize,
    pub novelty_distance_milli: u32,
    pub openai: OpenAiAnalysisConfig,
}

/// Explicit, secret-free configuration for hosted analysis augmentations.
///
/// The API key is intentionally never represented in this serializable type.
/// At runtime it is read only from `OPENAI_API_KEY`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenAiAnalysisConfig {
    /// Master opt-in. Subfeatures are inert while this is false.
    pub enabled: bool,
    /// Replace the local signed-feature hash with OpenAI embeddings for the
    /// already-safe finding projection used by secondary similarity cohorts.
    pub embeddings_enabled: bool,
    /// Add hosted labels to clusters after a local K-means fit.
    pub cluster_labels_enabled: bool,
    /// Run the semantic behavior judge over structured behavior facts only.
    pub semantic_judge_enabled: bool,
    pub embedding_model: String,
    pub chat_model: String,
    pub embedding_batch_size: usize,
    /// Stored as thousandths so configuration remains deterministic and Eq.
    pub minimum_failure_confidence_milli: u32,
    pub emit_abstentions: bool,
}

/// Secret-free process settings for the learned-assessment worker. Provider
/// permission and spend limits remain project-scoped durable policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AssessmentConfig {
    pub enabled: bool,
    pub poll_interval_ms: u64,
    pub estimated_attempt_cost_micros: u64,
    /// Directory containing a verified task-completion ONNX artifact. When
    /// absent, local-classifier releases fail closed instead of falling back
    /// to a cloud evaluator.
    pub local_model_artifact_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueryConfig {
    pub max_run_page: u32,
    pub max_span_page: u32,
    pub cached_pages: usize,
    pub blob_preview_bytes: usize,
    pub comparison_max_input_steps: usize,
    pub comparison_max_rows: usize,
    pub comparison_lookahead: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BlobConfig {
    pub inline_attribute_bytes: usize,
    pub allow_larger_local_reveal: bool,
    pub maximum_local_reveal_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    pub read_enabled: bool,
    pub compute_enabled: bool,
    pub write_enabled: bool,
    pub payload_reveal_enabled: bool,
    pub default_page_size: u32,
    pub maximum_page_size: u32,
    pub maximum_evidence_spans: u32,
    pub maximum_reveal_bytes: usize,
    pub maximum_response_bytes: usize,
    pub cursor_ttl_seconds: u64,
    pub job_poll_interval_ms: u64,
}

impl Default for PersevalConfigV1 {
    fn default() -> Self {
        let workspace_dir = ProjectDirs::from("dev", "perseval", "Perseval")
            .map(|dirs| dirs.data_dir().join("workspaces/default"))
            .unwrap_or_else(|| PathBuf::from(".perseval/default"));
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            workspace_id: "default".into(),
            workspace_dir,
            reviewer_ref: "local-human".into(),
            otlp: OtlpConfig::default(),
            stream: StreamConfig::default(),
            lifecycle: LifecycleConfig::default(),
            analysis: AnalysisConfig::default(),
            assessments: AssessmentConfig::default(),
            query: QueryConfig::default(),
            blobs: BlobConfig::default(),
            mcp: McpConfig::default(),
        }
    }
}

impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind_addr: "127.0.0.1:4318"
                .parse()
                .expect("valid default OTLP address"),
            source_id: "otlp-local".into(),
            max_wire_bytes: 16 * 1024 * 1024,
            max_decoded_bytes: 64 * 1024 * 1024,
            max_spans_per_request: 100_000,
            max_attributes_per_span: 1_024,
            retry_after_seconds: 1,
        }
    }
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            queue_batches: 64,
            queue_bytes: 256 * 1024 * 1024,
            microbatch_spans: 2_048,
            microbatch_bytes: 8 * 1024 * 1024,
            microbatch_wait_ms: 25,
            projection_retry_page: 64,
            projection_retry_initial_ms: 100,
            projection_retry_max_ms: 5_000,
            topology_chunk_rows: 2_048,
            pipeline_metrics_flush_ms: 1_000,
            delta_history: 4_096,
            subscriber_capacity: 256,
            ui_max_deltas_per_frame: 512,
        }
    }
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            idle_ms: 30_000,
            finalization_grace_ms: 5_000,
            sweep_ms: 1_000,
            shutdown_drain_ms: 5_000,
        }
    }
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            feature_similarity_enabled: false,
            cohort_rebuild_debounce_ms: 1_000,
            cohort_job_queue: 1_024,
            cohort_quality_sample_size: 512,
            cohort_rebuild_new_percent: 10,
            cohort_rebuild_new_cases: 250,
            cohort_rebuild_novelty_percent: 25,
            cohort_model_history: 3,
            cohort_feature_cache_entries: 20_000,
            cohort_maximum_cases: 20_000,
            embedding_dimensions: 256,
            maximum_clusters: 8,
            minimum_findings: 3,
            novelty_distance_milli: 350,
            openai: OpenAiAnalysisConfig::default(),
        }
    }
}

impl Default for OpenAiAnalysisConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            embeddings_enabled: false,
            cluster_labels_enabled: false,
            semantic_judge_enabled: false,
            embedding_model: "text-embedding-3-small".into(),
            chat_model: "gpt-5-mini".into(),
            embedding_batch_size: 128,
            minimum_failure_confidence_milli: 800,
            emit_abstentions: true,
        }
    }
}

impl Default for AssessmentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_ms: 100,
            estimated_attempt_cost_micros: 0,
            local_model_artifact_dir: None,
        }
    }
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            max_run_page: 200,
            max_span_page: 500,
            cached_pages: 8,
            blob_preview_bytes: 64 * 1024,
            comparison_max_input_steps: 100_000,
            comparison_max_rows: 1_024,
            comparison_lookahead: 32,
        }
    }
}

impl Default for BlobConfig {
    fn default() -> Self {
        Self {
            inline_attribute_bytes: DEFAULT_INLINE_ATTRIBUTE_BYTES,
            allow_larger_local_reveal: false,
            maximum_local_reveal_bytes: 16 * 1024 * 1024,
        }
    }
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            read_enabled: true,
            compute_enabled: false,
            write_enabled: false,
            payload_reveal_enabled: false,
            default_page_size: 50,
            maximum_page_size: 200,
            maximum_evidence_spans: 128,
            maximum_reveal_bytes: 4 * 1024,
            maximum_response_bytes: 2 * 1024 * 1024,
            cursor_ttl_seconds: 15 * 60,
            job_poll_interval_ms: 1_000,
        }
    }
}

impl PersevalConfigV1 {
    /// Returns the TOML file Perseval loads and saves for this user.
    ///
    /// `PERSEVAL_CONFIG` remains authoritative when present so the GUI never
    /// writes a surprising second configuration file.
    pub fn file_path() -> Result<PathBuf, ConfigError> {
        if let Some(path) = std::env::var_os(CONFIG_ENV) {
            return Ok(PathBuf::from(path));
        }
        ProjectDirs::from("dev", "perseval", "Perseval")
            .map(|dirs| dirs.config_dir().join("perseval.toml"))
            .ok_or_else(|| {
                ConfigError::Invalid("platform configuration directory is unavailable".into())
            })
    }

    pub fn load() -> Result<Self, ConfigError> {
        let explicit = std::env::var_os(CONFIG_ENV).map(PathBuf::from);
        let default_path = Self::file_path().ok();
        let path = explicit.clone().or(default_path);
        let mut config = match path.as_deref() {
            Some(path) if path.exists() => toml::from_str(&fs::read_to_string(path)?)?,
            Some(path) if explicit.is_some() => {
                return Err(ConfigError::Missing(path.to_path_buf()));
            }
            _ => Self::default(),
        };
        if let Some(path) = std::env::var_os(WORKSPACE_ENV) {
            config.workspace_dir = PathBuf::from(path);
        }
        if let Ok(value) = std::env::var(OTLP_ENABLED_ENV) {
            config.otlp.enabled = parse_bool(OTLP_ENABLED_ENV, &value)?;
        }
        if let Ok(value) = std::env::var(OTLP_BIND_ENV) {
            config.otlp.bind_addr = value.parse().map_err(|_| {
                ConfigError::Invalid(format!("{OTLP_BIND_ENV} is not a socket address"))
            })?;
        }
        if let Ok(value) = std::env::var(REVIEWER_REF_ENV) {
            config.reviewer_ref = value;
        }
        if let Ok(value) = std::env::var(OPENAI_ENABLED_ENV) {
            config.analysis.openai.enabled = parse_bool(OPENAI_ENABLED_ENV, &value)?;
        }
        for (name, target) in [
            (MCP_READ_ENABLED_ENV, &mut config.mcp.read_enabled),
            (MCP_COMPUTE_ENABLED_ENV, &mut config.mcp.compute_enabled),
            (MCP_WRITE_ENABLED_ENV, &mut config.mcp.write_enabled),
            (
                MCP_PAYLOAD_REVEAL_ENABLED_ENV,
                &mut config.mcp.payload_reveal_enabled,
            ),
        ] {
            if let Ok(value) = std::env::var(name) {
                *target = parse_bool(name, &value)?;
            }
        }
        config.validate()?;
        Ok(config)
    }

    /// Validates and atomically saves the configuration with private file
    /// permissions. Runtime-owned settings take effect on the next launch.
    pub fn save(&self) -> Result<PathBuf, ConfigError> {
        self.validate()?;
        let path = Self::file_path()?;
        self.save_to(&path)?;
        Ok(path)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate()?;
        let parent = path.parent().ok_or_else(|| {
            ConfigError::Invalid("configuration file has no parent directory".into())
        })?;
        fs::create_dir_all(parent)?;
        set_mode(parent, 0o700)?;

        let encoded = toml::to_string_pretty(self)?;
        let temporary = path.with_extension("toml.tmp");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        set_mode(&temporary, 0o600)?;
        file.write_all(encoded.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        set_mode(path, 0o600)?;
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(ConfigError::Invalid(format!(
                "unsupported config schema version {}",
                self.schema_version
            )));
        }
        if self.workspace_id.trim().is_empty() {
            return Err(ConfigError::Invalid("workspace_id cannot be empty".into()));
        }
        if self.reviewer_ref.trim().is_empty() {
            return Err(ConfigError::Invalid("reviewer_ref cannot be empty".into()));
        }
        if self.otlp.enabled && !self.otlp.bind_addr.ip().is_loopback() {
            return Err(ConfigError::Invalid(
                "non-loopback OTLP binding requires a future explicit unsafe-network setting"
                    .into(),
            ));
        }
        for (name, value) in [
            ("otlp.max_wire_bytes", self.otlp.max_wire_bytes),
            ("otlp.max_decoded_bytes", self.otlp.max_decoded_bytes),
            (
                "otlp.max_spans_per_request",
                self.otlp.max_spans_per_request,
            ),
            ("stream.queue_batches", self.stream.queue_batches),
            ("stream.queue_bytes", self.stream.queue_bytes),
            (
                "stream.subscriber_capacity",
                self.stream.subscriber_capacity,
            ),
            (
                "stream.ui_max_deltas_per_frame",
                self.stream.ui_max_deltas_per_frame,
            ),
            (
                "stream.pipeline_metrics_flush_ms",
                self.stream.pipeline_metrics_flush_ms as usize,
            ),
            (
                "stream.projection_retry_page",
                self.stream.projection_retry_page,
            ),
            (
                "stream.projection_retry_initial_ms",
                self.stream.projection_retry_initial_ms as usize,
            ),
            (
                "stream.projection_retry_max_ms",
                self.stream.projection_retry_max_ms as usize,
            ),
            (
                "stream.topology_chunk_rows",
                self.stream.topology_chunk_rows,
            ),
            ("query.max_run_page", self.query.max_run_page as usize),
            ("query.max_span_page", self.query.max_span_page as usize),
            ("query.cached_pages", self.query.cached_pages),
            (
                "analysis.embedding_dimensions",
                self.analysis.embedding_dimensions,
            ),
            (
                "analysis.cohort_rebuild_debounce_ms",
                self.analysis.cohort_rebuild_debounce_ms as usize,
            ),
            ("analysis.cohort_job_queue", self.analysis.cohort_job_queue),
            (
                "analysis.cohort_quality_sample_size",
                self.analysis.cohort_quality_sample_size,
            ),
            (
                "analysis.cohort_rebuild_new_percent",
                self.analysis.cohort_rebuild_new_percent as usize,
            ),
            (
                "analysis.cohort_rebuild_new_cases",
                self.analysis.cohort_rebuild_new_cases,
            ),
            (
                "analysis.cohort_rebuild_novelty_percent",
                self.analysis.cohort_rebuild_novelty_percent as usize,
            ),
            (
                "analysis.cohort_model_history",
                self.analysis.cohort_model_history,
            ),
            (
                "analysis.cohort_feature_cache_entries",
                self.analysis.cohort_feature_cache_entries,
            ),
            (
                "analysis.cohort_maximum_cases",
                self.analysis.cohort_maximum_cases,
            ),
            ("analysis.maximum_clusters", self.analysis.maximum_clusters),
            ("analysis.minimum_findings", self.analysis.minimum_findings),
            (
                "assessments.poll_interval_ms",
                self.assessments.poll_interval_ms as usize,
            ),
            (
                "analysis.openai.embedding_batch_size",
                self.analysis.openai.embedding_batch_size,
            ),
        ] {
            if value == 0 {
                return Err(ConfigError::Invalid(format!(
                    "{name} must be greater than zero"
                )));
            }
        }
        if self.analysis.novelty_distance_milli > 2_000 {
            return Err(ConfigError::Invalid(
                "analysis.novelty_distance_milli must be at most 2000".into(),
            ));
        }
        let openai = &self.analysis.openai;
        let any_openai_feature = openai.embeddings_enabled
            || openai.cluster_labels_enabled
            || openai.semantic_judge_enabled;
        if any_openai_feature && !openai.enabled {
            return Err(ConfigError::Invalid(
                "analysis.openai.enabled must be true before enabling an OpenAI feature".into(),
            ));
        }
        if openai.embeddings_enabled && !self.analysis.feature_similarity_enabled {
            return Err(ConfigError::Invalid(
                "OpenAI embeddings require analysis.feature_similarity_enabled".into(),
            ));
        }
        if openai.cluster_labels_enabled && !openai.embeddings_enabled {
            return Err(ConfigError::Invalid(
                "OpenAI cluster labels require OpenAI embeddings".into(),
            ));
        }
        if openai.embedding_model.trim().is_empty() || openai.chat_model.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "OpenAI model names cannot be empty".into(),
            ));
        }
        if openai.minimum_failure_confidence_milli > 1_000 {
            return Err(ConfigError::Invalid(
                "analysis.openai.minimum_failure_confidence_milli must be at most 1000".into(),
            ));
        }
        if openai.embeddings_enabled && self.analysis.embedding_dimensions > 3_072 {
            return Err(ConfigError::Invalid(
                "analysis.embedding_dimensions must not exceed 3072 for OpenAI embeddings".into(),
            ));
        }
        if self.analysis.cohort_rebuild_new_percent > 100 {
            return Err(ConfigError::Invalid(
                "analysis.cohort_rebuild_new_percent must be at most 100".into(),
            ));
        }
        if self.analysis.cohort_rebuild_novelty_percent > 100 {
            return Err(ConfigError::Invalid(
                "analysis.cohort_rebuild_novelty_percent must be at most 100".into(),
            ));
        }
        if self.analysis.cohort_maximum_cases < self.analysis.minimum_findings {
            return Err(ConfigError::Invalid(
                "analysis.cohort_maximum_cases must be at least minimum_findings".into(),
            ));
        }
        if self.stream.projection_retry_initial_ms > self.stream.projection_retry_max_ms {
            return Err(ConfigError::Invalid(
                "stream.projection_retry_initial_ms must not exceed projection_retry_max_ms".into(),
            ));
        }
        if self.mcp.default_page_size == 0
            || self.mcp.maximum_page_size == 0
            || self.mcp.maximum_evidence_spans == 0
            || self.mcp.maximum_reveal_bytes == 0
            || self.mcp.maximum_response_bytes == 0
            || self.mcp.cursor_ttl_seconds == 0
            || self.mcp.job_poll_interval_ms == 0
        {
            return Err(ConfigError::Invalid(
                "MCP bounds and intervals must be greater than zero".into(),
            ));
        }
        if self.mcp.default_page_size > self.mcp.maximum_page_size {
            return Err(ConfigError::Invalid(
                "mcp.default_page_size must not exceed mcp.maximum_page_size".into(),
            ));
        }
        if self.mcp.maximum_page_size > 200 {
            return Err(ConfigError::Invalid(
                "mcp.maximum_page_size must not exceed the protocol ceiling of 200".into(),
            ));
        }
        if self.mcp.maximum_evidence_spans > 128 {
            return Err(ConfigError::Invalid(
                "mcp.maximum_evidence_spans must not exceed the protocol ceiling of 128".into(),
            ));
        }
        if self.mcp.maximum_reveal_bytes > 65_536 {
            return Err(ConfigError::Invalid(
                "mcp.maximum_reveal_bytes must not exceed the protocol ceiling of 65536".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration file does not exist: {0}")]
    Missing(PathBuf),
    #[error("configuration I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("configuration parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("configuration serialization error: {0}")]
    TomlEncode(#[from] toml::ser::Error),
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), std::io::Error> {
    Ok(())
}

fn parse_bool(name: &str, value: &str) -> Result<bool, ConfigError> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::Invalid(format!("{name} is not a boolean"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_enable_only_local_collection() {
        let config = PersevalConfigV1::default();
        assert!(config.otlp.enabled);
        assert!(config.otlp.bind_addr.ip().is_loopback());
        assert!(!config.analysis.feature_similarity_enabled);
        assert!(!config.analysis.openai.enabled);
        assert!(!config.analysis.openai.embeddings_enabled);
        assert!(!config.analysis.openai.cluster_labels_enabled);
        assert!(!config.analysis.openai.semantic_judge_enabled);
        assert!(config.otlp.bind_addr.ip().is_loopback());
        assert_eq!(config.query.max_span_page, 500);
        config.validate().unwrap();
    }

    #[test]
    fn feature_similarity_config_serializes_truthfully_and_reads_legacy_name() {
        let config: AnalysisConfig = toml::from_str("semantic_clustering_enabled = false").unwrap();
        assert!(!config.feature_similarity_enabled);

        let encoded = toml::to_string(&config).unwrap();
        assert!(encoded.contains("feature_similarity_enabled = false"));
        assert!(!encoded.contains("semantic_clustering_enabled"));
    }

    #[test]
    fn feature_similarity_case_bound_cannot_disable_the_minimum_silently() {
        let mut config = PersevalConfigV1::default();
        config.analysis.cohort_maximum_cases = config.analysis.minimum_findings - 1;
        assert!(config.validate().is_err());
    }

    #[test]
    fn openai_features_require_explicit_master_and_dependencies() {
        let mut config = PersevalConfigV1::default();
        config.analysis.openai.semantic_judge_enabled = true;
        assert!(config.validate().is_err());

        config.analysis.openai.enabled = true;
        config.validate().unwrap();

        config.analysis.openai.embeddings_enabled = true;
        assert!(config.validate().is_err());
        config.analysis.feature_similarity_enabled = true;
        config.validate().unwrap();

        config.analysis.openai.embeddings_enabled = false;
        config.analysis.openai.cluster_labels_enabled = true;
        assert!(config.validate().is_err());
    }

    #[test]
    fn openai_configuration_contains_no_secret_field() {
        let encoded = toml::to_string(&PersevalConfigV1::default()).unwrap();
        assert!(!encoded.to_ascii_lowercase().contains("api_key"));
        assert!(!encoded.to_ascii_lowercase().contains("secret"));
    }

    #[test]
    fn configuration_save_is_private_and_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/perseval.toml");
        let config = PersevalConfigV1 {
            reviewer_ref: "settings-test".into(),
            otlp: OtlpConfig {
                enabled: true,
                ..OtlpConfig::default()
            },
            ..PersevalConfigV1::default()
        };

        config.save_to(&path).unwrap();
        let decoded: PersevalConfigV1 =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(decoded, config);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn mcp_defaults_are_read_only_and_protocol_bounded() {
        let config = PersevalConfigV1::default();
        assert!(config.mcp.read_enabled);
        assert!(!config.mcp.compute_enabled);
        assert!(!config.mcp.write_enabled);
        assert!(!config.mcp.payload_reveal_enabled);
        assert_eq!(config.mcp.maximum_page_size, 200);
        assert_eq!(config.mcp.maximum_evidence_spans, 128);
        config.validate().unwrap();
    }
}
