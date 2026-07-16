use perseval_service::TRACE_FILE_ENV;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeMode {
    Embedded,
    Fixture,
}

impl RuntimeMode {
    pub(super) fn detect() -> Self {
        if std::env::var_os(TRACE_FILE_ENV).is_some() {
            Self::Fixture
        } else {
            Self::Embedded
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_remain_distinct() {
        assert_ne!(RuntimeMode::Embedded, RuntimeMode::Fixture);
    }
}
