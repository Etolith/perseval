//! Verified installation and update management for local Task Completion models.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
use perseval_model_runtime::{RuntimeError, TaskCompletionModelManifestV1, verify_artifact};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const TASK_COMPLETION_MODEL_CATALOG_URL: &str =
    "https://huggingface.co/Etolith/perseval-task-completion/resolve/main/catalog.json";
pub const TASK_COMPLETION_MODEL_CATALOG_SCHEMA_VERSION: &str =
    "perseval.task_completion_model_catalog.v1";

const EXPECTED_REPOSITORY: &str = "Etolith/perseval-task-completion";
const INSTALL_RECEIPT_FILE: &str = ".perseval-install.json";
const INSTALL_RECEIPT_SCHEMA_VERSION: &str = "perseval.task_completion_model_install.v1";
const MAX_CATALOG_BYTES: usize = 64 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const NETWORK_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const CATALOG_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const ARTIFACT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDownloadFileV1 {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCompletionModelCatalogV1 {
    pub schema_version: String,
    pub release_version: String,
    pub channel: String,
    pub repository: String,
    pub revision: String,
    pub model_id: String,
    pub manifest_sha256: String,
    pub files: Vec<ModelDownloadFileV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedTaskCompletionModelV1 {
    pub artifact_dir: PathBuf,
    pub release_version: String,
    pub channel: String,
    pub model_id: String,
    pub revision: String,
}

impl ManagedTaskCompletionModelV1 {
    pub fn matches_catalog(&self, catalog: &TaskCompletionModelCatalogV1) -> bool {
        self.release_version == catalog.release_version
            && self.channel == catalog.channel
            && self.model_id == catalog.model_id
            && self.revision == catalog.revision
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ManagedInstallReceiptV1 {
    schema_version: String,
    release_version: String,
    channel: String,
    model_id: String,
    revision: String,
}

#[derive(Debug, Error)]
pub enum ModelManagementError {
    #[error("model download client could not be configured: {0}")]
    ClientConfiguration(#[source] reqwest::Error),
    #[error("model catalog could not be downloaded: {0}")]
    CatalogRequest(#[source] reqwest::Error),
    #[error("model catalog response could not be read: {0}")]
    CatalogRead(#[source] std::io::Error),
    #[error("model catalog returned HTTP {0}")]
    CatalogStatus(StatusCode),
    #[error("model catalog is larger than the supported limit")]
    CatalogTooLarge,
    #[error("model catalog is not valid JSON: {0}")]
    CatalogJson(#[source] serde_json::Error),
    #[error("model installation receipt is not valid JSON: {0}")]
    InstallReceiptJson(#[source] serde_json::Error),
    #[error("model installation receipt is invalid: {0}")]
    InvalidInstallReceipt(String),
    #[error("model catalog is invalid: {0}")]
    InvalidCatalog(String),
    #[error("model file {path} could not be downloaded: {source}")]
    ArtifactRequest {
        path: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("model file {path} response could not be read: {source}")]
    ArtifactRead {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("model file {path} returned HTTP {status}")]
    ArtifactStatus { path: String, status: StatusCode },
    #[error("model file {path} is larger than the catalog declares")]
    ArtifactTooLarge { path: String },
    #[error("model file {path} has size {actual}, expected {expected}")]
    ArtifactSize {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error("model file {path} failed its SHA-256 integrity check")]
    ArtifactHash { path: String },
    #[error("model installation could not access local storage: {0}")]
    Storage(#[from] std::io::Error),
    #[error("downloaded model did not pass runtime verification: {0}")]
    RuntimeVerification(#[from] RuntimeError),
    #[error("downloaded model ID differs from the catalog")]
    ModelIdMismatch,
    #[error("the existing installation at {0} differs from this release")]
    ExistingInstallMismatch(PathBuf),
    #[error("the platform model directory is unavailable")]
    ModelDirectoryUnavailable,
}

#[derive(Clone)]
pub struct TaskCompletionModelManager {
    client: Client,
    catalog_url: String,
    install_root: PathBuf,
}

impl TaskCompletionModelManager {
    pub fn production() -> Result<Self, ModelManagementError> {
        Self::new(TASK_COMPLETION_MODEL_CATALOG_URL, managed_model_root()?)
    }

    pub fn new(
        catalog_url: impl Into<String>,
        install_root: PathBuf,
    ) -> Result<Self, ModelManagementError> {
        let client = Client::builder()
            .connect_timeout(NETWORK_CONNECT_TIMEOUT)
            .build()
            .map_err(ModelManagementError::ClientConfiguration)?;
        Ok(Self {
            client,
            catalog_url: catalog_url.into(),
            install_root,
        })
    }

    pub fn latest_release(&self) -> Result<TaskCompletionModelCatalogV1, ModelManagementError> {
        let mut response = self
            .client
            .get(&self.catalog_url)
            .timeout(CATALOG_REQUEST_TIMEOUT)
            .send()
            .map_err(ModelManagementError::CatalogRequest)?;
        if !response.status().is_success() {
            return Err(ModelManagementError::CatalogStatus(response.status()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_CATALOG_BYTES as u64)
        {
            return Err(ModelManagementError::CatalogTooLarge);
        }
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            let count = response
                .read(&mut chunk)
                .map_err(ModelManagementError::CatalogRead)?;
            if count == 0 {
                break;
            }
            if bytes.len() + count > MAX_CATALOG_BYTES {
                return Err(ModelManagementError::CatalogTooLarge);
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        let catalog: TaskCompletionModelCatalogV1 =
            serde_json::from_slice(&bytes).map_err(ModelManagementError::CatalogJson)?;
        validate_catalog(&catalog)?;
        Ok(catalog)
    }

    pub fn install(
        &self,
        catalog: &TaskCompletionModelCatalogV1,
    ) -> Result<ManagedTaskCompletionModelV1, ModelManagementError> {
        validate_catalog(catalog)?;
        std::fs::create_dir_all(&self.install_root)?;
        let destination = self.install_root.join(&catalog.release_version);
        if destination.exists() {
            return inspect_catalog_install(&destination, catalog);
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let staging_path = self
            .install_root
            .join(format!(".installing-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&staging_path)?;
        let mut staging = StagingDirectory::new(staging_path);

        for file in &catalog.files {
            self.download_file(catalog, file, staging.path())?;
        }
        let (manifest, _) = verify_artifact(staging.path())?;
        if manifest.model_id != catalog.model_id {
            return Err(ModelManagementError::ModelIdMismatch);
        }
        write_install_receipt(staging.path(), catalog)?;
        std::fs::rename(staging.path(), &destination)?;
        staging.keep = true;
        inspect_catalog_install(&destination, catalog)
    }

    fn download_file(
        &self,
        catalog: &TaskCompletionModelCatalogV1,
        file: &ModelDownloadFileV1,
        staging: &Path,
    ) -> Result<(), ModelManagementError> {
        let url = format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            catalog.repository, catalog.revision, file.path
        );
        let mut response = self
            .client
            .get(url)
            .timeout(ARTIFACT_REQUEST_TIMEOUT)
            .send()
            .map_err(|source| ModelManagementError::ArtifactRequest {
                path: file.path.clone(),
                source,
            })?;
        if !response.status().is_success() {
            return Err(ModelManagementError::ArtifactStatus {
                path: file.path.clone(),
                status: response.status(),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length != file.size_bytes)
        {
            return Err(ModelManagementError::ArtifactSize {
                path: file.path.clone(),
                expected: file.size_bytes,
                actual: response.content_length().unwrap_or_default(),
            });
        }

        let destination = staging.join(&file.path);
        let mut output = std::fs::File::create(&destination)?;
        let mut hasher = Sha256::new();
        let mut actual = 0_u64;
        let mut chunk = [0_u8; 64 * 1024];
        loop {
            let count =
                response
                    .read(&mut chunk)
                    .map_err(|source| ModelManagementError::ArtifactRead {
                        path: file.path.clone(),
                        source,
                    })?;
            if count == 0 {
                break;
            }
            actual = actual.saturating_add(count as u64);
            if actual > file.size_bytes {
                return Err(ModelManagementError::ArtifactTooLarge {
                    path: file.path.clone(),
                });
            }
            hasher.update(&chunk[..count]);
            output.write_all(&chunk[..count])?;
        }
        output.sync_all()?;
        if actual != file.size_bytes {
            return Err(ModelManagementError::ArtifactSize {
                path: file.path.clone(),
                expected: file.size_bytes,
                actual,
            });
        }
        if format!("sha256:{}", hex::encode(hasher.finalize())) != file.sha256 {
            return Err(ModelManagementError::ArtifactHash {
                path: file.path.clone(),
            });
        }
        Ok(())
    }
}

pub fn managed_model_root() -> Result<PathBuf, ModelManagementError> {
    ProjectDirs::from("dev", "perseval", "Perseval")
        .map(|dirs| dirs.data_dir().join("models").join("task-completion"))
        .ok_or(ModelManagementError::ModelDirectoryUnavailable)
}

pub fn inspect_managed_model(
    artifact_dir: &Path,
) -> Result<TaskCompletionModelManifestV1, ModelManagementError> {
    verify_artifact(artifact_dir)
        .map(|(manifest, _)| manifest)
        .map_err(ModelManagementError::RuntimeVerification)
}

pub fn inspect_managed_install(
    artifact_dir: &Path,
) -> Result<ManagedTaskCompletionModelV1, ModelManagementError> {
    let path = artifact_dir.join(INSTALL_RECEIPT_FILE);
    let bytes = std::fs::read(path)?;
    let receipt: ManagedInstallReceiptV1 =
        serde_json::from_slice(&bytes).map_err(ModelManagementError::InstallReceiptJson)?;
    validate_install_receipt(&receipt)?;
    Ok(ManagedTaskCompletionModelV1 {
        artifact_dir: artifact_dir.to_path_buf(),
        release_version: receipt.release_version,
        channel: receipt.channel,
        model_id: receipt.model_id,
        revision: receipt.revision,
    })
}

fn inspect_catalog_install(
    artifact_dir: &Path,
    catalog: &TaskCompletionModelCatalogV1,
) -> Result<ManagedTaskCompletionModelV1, ModelManagementError> {
    let installed = inspect_managed_install(artifact_dir)?;
    if !installed.matches_catalog(catalog) {
        return Err(ModelManagementError::ExistingInstallMismatch(
            artifact_dir.to_path_buf(),
        ));
    }
    for file in &catalog.files {
        verify_file(artifact_dir, file)?;
    }
    let (manifest, _) = verify_artifact(artifact_dir)?;
    if manifest.model_id != catalog.model_id {
        return Err(ModelManagementError::ModelIdMismatch);
    }
    Ok(installed)
}

fn verify_file(
    artifact_dir: &Path,
    file: &ModelDownloadFileV1,
) -> Result<(), ModelManagementError> {
    let path = artifact_dir.join(&file.path);
    let mut input = File::open(&path)?;
    let metadata_size = input.metadata()?.len();
    if metadata_size != file.size_bytes {
        return Err(ModelManagementError::ArtifactSize {
            path: file.path.clone(),
            expected: file.size_bytes,
            actual: metadata_size,
        });
    }
    let mut hasher = Sha256::new();
    let mut actual = 0_u64;
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        actual = actual.saturating_add(count as u64);
        if actual > file.size_bytes {
            return Err(ModelManagementError::ArtifactTooLarge {
                path: file.path.clone(),
            });
        }
        hasher.update(&chunk[..count]);
    }
    if actual != file.size_bytes {
        return Err(ModelManagementError::ArtifactSize {
            path: file.path.clone(),
            expected: file.size_bytes,
            actual,
        });
    }
    let digest = format!("sha256:{}", hex::encode(hasher.finalize()));
    if digest != file.sha256 {
        return Err(ModelManagementError::ArtifactHash {
            path: file.path.clone(),
        });
    }
    Ok(())
}

fn write_install_receipt(
    artifact_dir: &Path,
    catalog: &TaskCompletionModelCatalogV1,
) -> Result<(), ModelManagementError> {
    let receipt = ManagedInstallReceiptV1 {
        schema_version: INSTALL_RECEIPT_SCHEMA_VERSION.into(),
        release_version: catalog.release_version.clone(),
        channel: catalog.channel.clone(),
        model_id: catalog.model_id.clone(),
        revision: catalog.revision.clone(),
    };
    let bytes =
        serde_json::to_vec_pretty(&receipt).map_err(ModelManagementError::InstallReceiptJson)?;
    let mut output = File::create(artifact_dir.join(INSTALL_RECEIPT_FILE))?;
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    Ok(())
}

fn validate_install_receipt(receipt: &ManagedInstallReceiptV1) -> Result<(), ModelManagementError> {
    let invalid = |message: &str| ModelManagementError::InvalidInstallReceipt(message.to_string());
    if receipt.schema_version != INSTALL_RECEIPT_SCHEMA_VERSION {
        return Err(invalid("unsupported schema version"));
    }
    if !safe_segment(&receipt.release_version) {
        return Err(invalid("unsafe release version"));
    }
    if !matches!(receipt.channel.as_str(), "development" | "stable") {
        return Err(invalid("unsupported release channel"));
    }
    if receipt.model_id.trim().is_empty() {
        return Err(invalid("model ID is empty"));
    }
    if receipt.revision.len() != 40
        || !receipt
            .revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid("revision must be an immutable commit hash"));
    }
    Ok(())
}

fn validate_catalog(catalog: &TaskCompletionModelCatalogV1) -> Result<(), ModelManagementError> {
    let invalid = |message: &str| ModelManagementError::InvalidCatalog(message.into());
    if catalog.schema_version != TASK_COMPLETION_MODEL_CATALOG_SCHEMA_VERSION {
        return Err(invalid("unsupported schema version"));
    }
    if catalog.repository != EXPECTED_REPOSITORY {
        return Err(invalid("unexpected model repository"));
    }
    if !safe_segment(&catalog.release_version) {
        return Err(invalid("unsafe release version"));
    }
    if !matches!(catalog.channel.as_str(), "development" | "stable") {
        return Err(invalid("unsupported release channel"));
    }
    if catalog.revision.len() != 40
        || !catalog
            .revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid("revision must be an immutable commit hash"));
    }
    if catalog.model_id.trim().is_empty() {
        return Err(invalid("model ID is empty"));
    }
    if !valid_digest(&catalog.manifest_sha256) {
        return Err(invalid("manifest digest is invalid"));
    }
    if catalog.files.is_empty() {
        return Err(invalid("catalog has no files"));
    }
    let mut names = HashSet::new();
    let mut manifest = None;
    for file in &catalog.files {
        let relative = Path::new(&file.path);
        if !safe_segment(&file.path)
            || relative.components().count() != 1
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(invalid("catalog contains an unsafe file path"));
        }
        if !names.insert(file.path.as_str()) {
            return Err(invalid("catalog contains a duplicate file"));
        }
        if file.path == INSTALL_RECEIPT_FILE {
            return Err(invalid("catalog contains a reserved file name"));
        }
        if file.size_bytes == 0 || file.size_bytes > MAX_ARTIFACT_BYTES {
            return Err(invalid("catalog file size is outside the supported range"));
        }
        if !valid_digest(&file.sha256) {
            return Err(invalid("catalog file digest is invalid"));
        }
        if file.path == "manifest.json" {
            manifest = Some(file);
        }
    }
    let manifest = manifest.ok_or_else(|| invalid("catalog does not include manifest.json"))?;
    if manifest.sha256 != catalog.manifest_sha256 {
        return Err(invalid("manifest digest does not match its file record"));
    }
    Ok(())
}

fn safe_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn valid_digest(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

struct StagingDirectory {
    path: PathBuf,
    keep: bool,
}

impl StagingDirectory {
    fn new(path: PathBuf) -> Self {
        Self { path, keep: false }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> TaskCompletionModelCatalogV1 {
        TaskCompletionModelCatalogV1 {
            schema_version: TASK_COMPLETION_MODEL_CATALOG_SCHEMA_VERSION.into(),
            release_version: "runtime-candidate-v1".into(),
            channel: "development".into(),
            repository: EXPECTED_REPOSITORY.into(),
            revision: "861e0029d08e93d363b49340d1774c3eece7d75f".into(),
            model_id: "model@sha".into(),
            manifest_sha256: format!("sha256:{}", "a".repeat(64)),
            files: vec![ModelDownloadFileV1 {
                path: "manifest.json".into(),
                size_bytes: 100,
                sha256: format!("sha256:{}", "a".repeat(64)),
            }],
        }
    }

    #[test]
    fn catalog_requires_immutable_revision_and_safe_paths() {
        let mut candidate = catalog();
        validate_catalog(&candidate).unwrap();

        candidate.revision = "main".into();
        assert!(matches!(
            validate_catalog(&candidate),
            Err(ModelManagementError::InvalidCatalog(_))
        ));
        candidate = catalog();
        candidate.files[0].path = "../manifest.json".into();
        assert!(matches!(
            validate_catalog(&candidate),
            Err(ModelManagementError::InvalidCatalog(_))
        ));
        candidate = catalog();
        candidate.files[0].path = "manifest\\escape.json".into();
        assert!(matches!(
            validate_catalog(&candidate),
            Err(ModelManagementError::InvalidCatalog(_))
        ));
    }

    #[test]
    fn catalog_manifest_digest_must_match_file_record() {
        let mut candidate = catalog();
        candidate.manifest_sha256 = format!("sha256:{}", "b".repeat(64));
        assert!(matches!(
            validate_catalog(&candidate),
            Err(ModelManagementError::InvalidCatalog(_))
        ));
    }

    #[test]
    fn existing_file_integrity_rejects_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = b"known bytes";
        std::fs::write(directory.path().join("model.onnx"), bytes).unwrap();
        let file = ModelDownloadFileV1 {
            path: "model.onnx".into(),
            size_bytes: bytes.len() as u64,
            sha256: format!("sha256:{}", hex::encode(Sha256::digest(bytes))),
        };
        verify_file(directory.path(), &file).unwrap();
        std::fs::write(directory.path().join("model.onnx"), b"changed").unwrap();
        assert!(verify_file(directory.path(), &file).is_err());
    }

    #[test]
    fn managed_install_identity_includes_release_and_revision() {
        let directory = tempfile::tempdir().unwrap();
        let expected = catalog();
        write_install_receipt(directory.path(), &expected).unwrap();

        let installed = inspect_managed_install(directory.path()).unwrap();

        assert!(installed.matches_catalog(&expected));
        let mut newer_release = expected.clone();
        newer_release.release_version = "runtime-candidate-v2".into();
        assert!(!installed.matches_catalog(&newer_release));
        let mut newer_revision = expected;
        newer_revision.revision = "b".repeat(40);
        assert!(!installed.matches_catalog(&newer_revision));
    }

    #[test]
    fn existing_install_preserves_storage_errors() {
        let directory = tempfile::tempdir().unwrap();
        let expected = catalog();
        let destination = directory.path().join(&expected.release_version);
        std::fs::create_dir(&destination).unwrap();
        write_install_receipt(&destination, &expected).unwrap();
        let manager = TaskCompletionModelManager::new(
            TASK_COMPLETION_MODEL_CATALOG_URL,
            directory.path().to_path_buf(),
        )
        .unwrap();

        let error = manager.install(&expected).unwrap_err();

        assert!(matches!(error, ModelManagementError::Storage(_)));
    }

    #[test]
    #[ignore = "requires the public Hugging Face model repository"]
    fn published_candidate_downloads_and_passes_runtime_verification() {
        let directory = tempfile::tempdir().unwrap();
        let manager = TaskCompletionModelManager::new(
            TASK_COMPLETION_MODEL_CATALOG_URL,
            directory.path().to_path_buf(),
        )
        .unwrap();
        let catalog = manager.latest_release().unwrap();
        let installed = manager.install(&catalog).unwrap();

        assert_eq!(installed.model_id, catalog.model_id);
        assert_eq!(installed.revision, catalog.revision);
        assert_eq!(
            inspect_managed_model(&installed.artifact_dir)
                .unwrap()
                .model_id,
            catalog.model_id
        );
    }
}
