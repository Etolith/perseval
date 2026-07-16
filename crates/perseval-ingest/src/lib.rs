#![forbid(unsafe_code)]

//! OTLP ingestion for the Perseval local trace workbench.

pub mod otlp;

/// The transport by which a source enters Perseval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestTransport {
    Filesystem,
    OtlpHttp,
}

/// Stable metadata recorded for every configured source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDescriptor {
    pub source_id: String,
    pub adapter_id: String,
    pub adapter_version: String,
    pub transport: IngestTransport,
}

impl SourceDescriptor {
    pub fn new(
        source_id: impl Into<String>,
        adapter_id: impl Into<String>,
        adapter_version: impl Into<String>,
        transport: IngestTransport,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            adapter_id: adapter_id.into(),
            adapter_version: adapter_version.into(),
            transport,
        }
    }
}
