#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyticsBackend {
    Sqlite,
    DuckDbParquet,
}

/// Large scans, recurrence aggregation, and behavior-shape feature extraction.
pub trait AnalyticsStore: Send + Sync {
    fn backend(&self) -> AnalyticsBackend;
}
