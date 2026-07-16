use std::collections::BTreeSet;
use std::error::Error;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const SOURCE_MANIFEST_SCHEMA_VERSION: &str = "perseval.benchmark_source_manifest.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceManifest {
    pub schema_version: String,
    pub dataset: String,
    pub revision: String,
    pub artifact: String,
    pub url: String,
    pub sha256: String,
    pub rows: u64,
    pub resolved_rows: u64,
    pub unresolved_rows: u64,
    pub fixture_schema_version: String,
    pub selection_schema_version: String,
    pub tiers: Vec<BenchmarkTier>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkTier {
    pub name: String,
    pub purpose: String,
    pub resolved: Option<u64>,
    pub unresolved: Option<u64>,
    pub selection: String,
}

impl SourceManifest {
    pub fn load(path: &Path) -> Result<Self, Box<dyn Error>> {
        let manifest: Self = serde_json::from_reader(BufReader::new(File::open(path)?))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), Box<dyn Error>> {
        if self.schema_version != SOURCE_MANIFEST_SCHEMA_VERSION {
            return Err(format!(
                "unsupported source manifest schema {:?}; expected {:?}",
                self.schema_version, SOURCE_MANIFEST_SCHEMA_VERSION
            )
            .into());
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("source manifest sha256 must contain 64 hexadecimal characters".into());
        }
        if self.rows != self.resolved_rows + self.unresolved_rows {
            return Err("source row counts do not add up".into());
        }
        if self.dataset.trim().is_empty()
            || self.revision.trim().is_empty()
            || self.artifact.trim().is_empty()
            || self.fixture_schema_version.trim().is_empty()
            || self.selection_schema_version.trim().is_empty()
        {
            return Err("source manifest identities and schema versions cannot be empty".into());
        }
        if !self.url.starts_with("https://") {
            return Err("source manifest URL must use HTTPS".into());
        }
        if self.tiers.is_empty() {
            return Err("source manifest must define at least one benchmark tier".into());
        }
        let mut names = BTreeSet::new();
        for tier in &self.tiers {
            if tier.name.trim().is_empty()
                || tier.purpose.trim().is_empty()
                || tier.selection.trim().is_empty()
            {
                return Err("benchmark tier fields cannot be empty".into());
            }
            if !names.insert(&tier.name) {
                return Err(format!("duplicate benchmark tier {:?}", tier.name).into());
            }
            match (tier.resolved, tier.unresolved) {
                (Some(resolved), Some(unresolved))
                    if resolved <= self.resolved_rows && unresolved <= self.unresolved_rows => {}
                (None, None) => {}
                (Some(_), Some(_)) => {
                    return Err(
                        format!("benchmark tier {:?} exceeds source counts", tier.name).into(),
                    );
                }
                _ => {
                    return Err(format!(
                        "benchmark tier {:?} must specify both class counts or neither",
                        tier.name
                    )
                    .into());
                }
            }
        }
        Ok(())
    }

    pub fn tier(&self, name: &str) -> Result<&BenchmarkTier, Box<dyn Error>> {
        self.tiers
            .iter()
            .find(|tier| tier.name == name)
            .ok_or_else(|| format!("unknown benchmark tier {name:?}").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_inconsistent_source_counts() {
        let manifest = SourceManifest {
            schema_version: SOURCE_MANIFEST_SCHEMA_VERSION.into(),
            dataset: "dataset".into(),
            revision: "revision".into(),
            artifact: "artifact".into(),
            url: "https://example.test/artifact".into(),
            sha256: "0".repeat(64),
            rows: 3,
            resolved_rows: 1,
            unresolved_rows: 1,
            fixture_schema_version: "fixture-v1".into(),
            selection_schema_version: "selection-v1".into(),
            tiers: vec![BenchmarkTier {
                name: "ci".into(),
                purpose: "test".into(),
                resolved: Some(1),
                unresolved: Some(1),
                selection: "hash".into(),
            }],
        };

        assert!(manifest.validate().is_err());
    }
}
