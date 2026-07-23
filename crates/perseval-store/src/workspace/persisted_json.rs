use duckdb::{Error, types::Type};
use serde::de::DeserializeOwned;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("invalid JSON in persisted {field}: {source}")]
struct PersistedJsonColumnError {
    field: &'static str,
    #[source]
    source: serde_json::Error,
}

pub(super) fn decode_json_column<T: DeserializeOwned>(
    encoded: &str,
    column_index: usize,
    field: &'static str,
) -> duckdb::Result<T> {
    serde_json::from_str(encoded).map_err(|source| {
        Error::FromSqlConversionFailure(
            column_index,
            Type::Text,
            Box::new(PersistedJsonColumnError { field, source }),
        )
    })
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn invalid_persisted_json_is_not_replaced_with_an_empty_value() {
        let error = decode_json_column::<Value>("{not-json", 3, "span attributes").unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("invalid JSON in persisted span attributes"));
    }
}
