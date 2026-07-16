use std::fmt;
use std::io::Write;
use std::path::Path;

use super::WorkbenchStateV1;
use super::state::WORKBENCH_STATE_VERSION;

#[derive(Debug)]
pub enum PersistenceError {
    Io(std::io::Error),
    Json(serde_json::Error),
    UnsupportedVersion(u32),
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "could not persist workbench state: {error}"),
            Self::Json(error) => write!(formatter, "invalid workbench state: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported workbench state version {version}")
            }
        }
    }
}

impl std::error::Error for PersistenceError {}

impl From<serde_json::Error> for PersistenceError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<std::io::Error> for PersistenceError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

pub fn encode_state(state: &WorkbenchStateV1) -> Result<Vec<u8>, PersistenceError> {
    Ok(serde_json::to_vec_pretty(state)?)
}

pub fn decode_state(bytes: &[u8]) -> Result<WorkbenchStateV1, PersistenceError> {
    let state: WorkbenchStateV1 = serde_json::from_slice(bytes)?;
    if state.schema_version != WORKBENCH_STATE_VERSION {
        return Err(PersistenceError::UnsupportedVersion(state.schema_version));
    }
    Ok(state)
}

pub fn load_state(path: &Path) -> Result<Option<WorkbenchStateV1>, PersistenceError> {
    match std::fs::read(path) {
        Ok(bytes) => decode_state(&bytes).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn save_state(path: &Path, state: &WorkbenchStateV1) -> Result<(), PersistenceError> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workbench state path has no parent directory",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    set_mode(parent, 0o700)?;
    let temporary = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    set_mode(&temporary, 0o600)?;
    file.write_all(&encode_state(state)?)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    set_mode(path, 0o600)?;
    if let Ok(directory) = std::fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn state_round_trips_with_a_version_gate() {
        let state = WorkbenchStateV1::default();
        let bytes = encode_state(&state).unwrap();
        assert_eq!(decode_state(&bytes).unwrap(), state);
    }

    #[test]
    fn future_versions_are_rejected() {
        let bytes = br#"{"schema_version":999,"active_activity":"failures","editors":[],"active_editor":null,"panes":{"primary_sidebar_visible":false,"inspector_visible":false,"bottom_panel_visible":false,"primary_sidebar_width":280.0,"inspector_width":360.0,"bottom_panel_height":220.0},"scope":{"project":"all_projects","environment":null,"build":null,"session":null,"time_range":null},"onboarding":{"completed":false,"dismissed":false,"current_step":0},"bulk_selection":{"failure_group_ids":[]},"focus":"editor"}"#;
        assert!(matches!(
            decode_state(bytes),
            Err(PersistenceError::UnsupportedVersion(999))
        ));
    }

    #[test]
    fn state_file_is_atomic_and_private() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("state.json");
        let state = WorkbenchStateV1::default();

        save_state(&path, &state).unwrap();

        assert_eq!(load_state(&path).unwrap(), Some(state));
        assert!(!path.with_extension("json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
