use std::collections::BTreeSet;

pub(crate) fn telemetry_gap_summary(gaps: &[String]) -> String {
    let kinds = gaps
        .iter()
        .filter_map(|gap| gap.rsplit(':').next())
        .map(|kind| kind.replace('_', " "))
        .collect::<BTreeSet<_>>();
    let visible = kinds.iter().take(4).cloned().collect::<Vec<_>>();
    let remaining = kinds.len().saturating_sub(visible.len());
    let kinds = if visible.is_empty() {
        "unspecified telemetry".into()
    } else if remaining == 0 {
        visible.join(", ")
    } else {
        format!("{} + {remaining} more kinds", visible.join(", "))
    };
    format!("{} missing observations · {kinds}", gaps.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_repeated_gap_identities_without_rendering_payloads() {
        let gaps = vec![
            "trace:a:call:1:status_unknown".into(),
            "trace:a:call:2:status_unknown".into(),
            "trace:a:call:2:effect_unknown".into(),
        ];
        assert_eq!(
            telemetry_gap_summary(&gaps),
            "3 missing observations · effect unknown, status unknown"
        );
    }
}
