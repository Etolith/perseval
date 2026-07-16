use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceStoreLayout {
    root: PathBuf,
}

impl WorkspaceStoreLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn control_database(&self) -> PathBuf {
        self.root.join("control.sqlite3")
    }

    pub fn blob_directory(&self) -> PathBuf {
        self.root.join("blobs")
    }

    pub fn analytics_directory(&self) -> PathBuf {
        self.root.join("analytics")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_control_blobs_and_analytics() {
        let layout = WorkspaceStoreLayout::new("workspace");

        assert_eq!(
            layout.control_database(),
            PathBuf::from("workspace/control.sqlite3")
        );
        assert_eq!(layout.blob_directory(), PathBuf::from("workspace/blobs"));
        assert_eq!(
            layout.analytics_directory(),
            PathBuf::from("workspace/analytics")
        );
    }
}
