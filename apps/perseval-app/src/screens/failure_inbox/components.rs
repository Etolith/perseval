use super::*;
pub(super) use crate::components::{button, button_state, execution_tag, tag};

pub(super) const FULL_TRACE_DEPTH_INDENT: f32 = Geometry::TREE_INDENT;
const FULL_TRACE_DISCLOSURE_GUTTER: f32 = 20.;
const FULL_TRACE_STATUS_RAIL: f32 = 224.;
const FULL_TRACE_TIMELINE_LABEL: f32 = 320.;
const FULL_TRACE_TIMELINE_ROLE_MAX: f32 = 128.;

pub(super) fn tab_button(
    label: &str,
    active: bool,
    tab: InspectorTab,
    cx: &Context<FailureInbox>,
) -> gpui::Stateful<Div> {
    button(label, active)
        .id(("inspector-tab", tab as usize))
        .role(Role::Tab)
        .aria_label(label.to_string())
        .aria_selected(active)
        .tab_index(0)
        .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
        .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
}

pub(super) fn kv(label: &str, value: &str) -> Div {
    div()
        .mt_4()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(Theme::DIM)
                .child(label.to_uppercase()),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(Theme::MUTED)
                .child(value.to_string()),
        )
}

pub(super) fn trend_sparkline(
    values: &[u64],
    recurrence: Option<&FailureRecurrenceSeriesV1>,
) -> gpui::Stateful<Div> {
    let max = values.iter().copied().max().unwrap_or(1).max(1) as f32;
    let bar_values = recurrence.map_or_else(
        || {
            values
                .iter()
                .map(|value| Some(*value as f32 / max))
                .collect::<Vec<_>>()
        },
        |series| {
            series
                .buckets
                .iter()
                .map(|bucket| {
                    bucket
                        .recurrence_rate_basis_points
                        .map(|rate| rate as f32 / 10_000.)
                })
                .collect::<Vec<_>>()
        },
    );
    let denominator_backed = recurrence.is_some();
    let (_, tint) = recurrence.map_or_else(|| trend_summary(values), recurrence_trend_summary);
    let (label, accessible_label) = recurrence.map_or_else(
        || {
            let (summary, _) = trend_summary(values);
            (summary.clone(), format!("Occurrence trend: {summary}"))
        },
        |series| {
            let (trend, _) = recurrence_trend_summary(series);
            let eligible = series
                .buckets
                .iter()
                .map(|bucket| bucket.eligible_run_count)
                .sum::<u64>();
            let affected = series
                .buckets
                .iter()
                .map(|bucket| bucket.affected_run_count)
                .sum::<u64>();
            let findings = series
                .buckets
                .iter()
                .map(|bucket| bucket.finding_count)
                .sum::<u64>();
            let bucket_rates = series
                .buckets
                .iter()
                .map(|bucket| {
                    bucket.recurrence_rate_basis_points.map_or_else(
                        || "no eligible runs".to_string(),
                        |rate| format!("{}%", u32::from(rate) / 100),
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            (
                format!("{trend} · {affected} / {eligible} runs"),
                format!(
                    "Recurrence from {} through {}: {affected} affected of {eligible} eligible runs, {findings} findings across {} equal buckets. Bucket rates: {bucket_rates}",
                    series.started_at_unix_nano,
                    series.ended_at_unix_nano,
                    series.buckets.len()
                ),
            )
        },
    );
    div()
        .id(format!("failure-trend-{bar_values:?}"))
        .role(Role::Status)
        .aria_label(accessible_label)
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .overflow_hidden()
        .child(
            div()
                .w(px(58.))
                .h(px(22.))
                .flex_none()
                .flex()
                .items_end()
                .justify_between()
                .children(bar_values.iter().enumerate().map(|(index, value)| {
                    let (height, color) = value.map_or((2., Theme::BORDER), |rate| {
                        let color = if denominator_backed {
                            if rate <= f32::EPSILON {
                                Theme::GREEN
                            } else {
                                Theme::RED
                            }
                        } else {
                            tint
                        };
                        (4. + 18. * rate.clamp(0., 1.), color)
                    });
                    div()
                        .id(("failure-trend-bar", index))
                        .w(px(6.))
                        .h(px(height))
                        .rounded(px(1.5))
                        .bg(color)
                })),
        )
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_xs()
                .text_color(tint)
                .child(label),
        )
}

fn recurrence_trend_summary(series: &FailureRecurrenceSeriesV1) -> (String, Rgba) {
    if series.buckets.len() < 2 {
        return ("New".into(), Theme::CYAN);
    }
    let comparison_width = series.buckets.len() / 2;
    let (earlier_affected, earlier_eligible) =
        recurrence_totals(&series.buckets[..comparison_width]);
    let (later_affected, later_eligible) =
        recurrence_totals(&series.buckets[series.buckets.len() - comparison_width..]);
    if later_eligible == 0 {
        return ("No recent runs".into(), Theme::DIM);
    }
    if earlier_eligible == 0 {
        return ("New".into(), Theme::CYAN);
    }
    let earlier_rate = earlier_affected as f64 / earlier_eligible as f64;
    let later_rate = later_affected as f64 / later_eligible as f64;
    if earlier_rate == 0. {
        return if later_rate > 0. {
            ("New".into(), Theme::RED)
        } else {
            ("Flat".into(), Theme::DIM)
        };
    }
    let change = ((later_rate - earlier_rate) / earlier_rate * 100.).round() as i64;
    if change > 0 {
        (format!("+{change}%"), Theme::RED)
    } else if change < 0 {
        (format!("{change}%"), Theme::GREEN)
    } else if series
        .buckets
        .iter()
        .filter_map(|bucket| bucket.recurrence_rate_basis_points)
        .collect::<Vec<_>>()
        .windows(2)
        .any(|pair| pair[0] != pair[1])
    {
        ("Intermittent".into(), Theme::AMBER)
    } else {
        ("Flat".into(), Theme::DIM)
    }
}

fn recurrence_totals(buckets: &[FailureRecurrenceBucketV1]) -> (u64, u64) {
    buckets.iter().fold((0, 0), |(affected, eligible), bucket| {
        (
            affected.saturating_add(bucket.affected_run_count),
            eligible.saturating_add(bucket.eligible_run_count),
        )
    })
}

fn trend_summary(values: &[u64]) -> (String, Rgba) {
    if values.len() < 2 || values.iter().sum::<u64>() <= 1 {
        return ("New".into(), Theme::CYAN);
    }
    let comparison_width = values.len() / 2;
    let earlier = values[..comparison_width].iter().sum::<u64>();
    let later = values[values.len() - comparison_width..]
        .iter()
        .sum::<u64>();
    if earlier == 0 {
        return ("New".into(), Theme::CYAN);
    }
    let change = ((later as f64 - earlier as f64) / earlier as f64 * 100.).round() as i64;
    if change > 0 {
        (format!("+{change}%"), Theme::RED)
    } else if change < 0 {
        (format!("{change}%"), Theme::GREEN)
    } else if values.windows(2).any(|pair| pair[0] != pair[1]) {
        ("Intermittent".into(), Theme::AMBER)
    } else {
        ("Flat".into(), Theme::DIM)
    }
}

#[derive(Clone, Copy)]
pub(super) struct FullTraceRowState {
    evidence: bool,
    search_match: bool,
    selected: bool,
    compact: bool,
    row_height: f32,
}

impl FullTraceRowState {
    pub(super) fn new(
        evidence: bool,
        search_match: bool,
        selected: bool,
        compact: bool,
        row_height: f32,
    ) -> Self {
        Self {
            evidence,
            search_match,
            selected,
            compact,
            row_height,
        }
    }
}

pub(super) fn full_trace_span_row(
    span: &SpanRow,
    state: FullTraceRowState,
    expanded: bool,
    cx: &Context<FailureInbox>,
) -> Div {
    let FullTraceRowState {
        evidence,
        search_match,
        selected,
        compact,
        row_height,
    } = state;
    let span_id = span.span_id.clone();
    let selected_span = span.clone();
    let has_children = span.has_children;
    let role = full_trace_role(span);
    let operation = full_trace_operation(span);
    let (status, status_tint) = span_status(span);
    let metadata_tint = if selected || evidence {
        Theme::MUTED
    } else {
        Theme::DIM
    };
    div()
        .w_full()
        .h(px(row_height))
        .pl(px(8. + FULL_TRACE_DEPTH_INDENT * span.depth as f32))
        .pr_2()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(Theme::BORDER)
        .bg(if selected {
            Theme::SELECTED
        } else if evidence {
            Theme::WARNING_SURFACE
        } else if search_match {
            Theme::INFO_SURFACE
        } else {
            Theme::BG
        })
        .child(
            div()
                .id(format!("tree-disclosure-{}", span.span_id))
                .when(has_children, |disclosure| {
                    disclosure
                        .role(Role::DisclosureTriangle)
                        .aria_label(format!(
                            "{} children for {}",
                            if expanded { "Collapse" } else { "Expand" },
                            span.name
                        ))
                        .aria_expanded(expanded)
                        .tab_index(0)
                        .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                })
                .w(px(FULL_TRACE_DISCLOSURE_GUTTER))
                .h(px(28.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .text_xs()
                .text_color(if has_children {
                    Theme::MUTED
                } else {
                    Theme::BORDER
                })
                .when(has_children, |disclosure| {
                    disclosure
                        .cursor_pointer()
                        .hover(|style| style.bg(Theme::PANEL_ALT))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_full_trace_span(span_id.clone(), true, cx)
                        }))
                })
                .child(if has_children {
                    if expanded { "▾" } else { "▸" }
                } else {
                    "·"
                }),
        )
        .child(
            div()
                .id(format!("tree-span-{}", span.span_id))
                .role(Role::TreeItem)
                .aria_label(format!(
                    "{}; role {}; operation {}; status {}; {:.1} milliseconds; {}{}",
                    span.name,
                    role,
                    operation,
                    status,
                    span.duration_nano as f64 / 1_000_000.,
                    if has_children {
                        if expanded {
                            "children expanded"
                        } else {
                            "children collapsed"
                        }
                    } else {
                        "no children"
                    },
                    if evidence { "; evidence" } else { "" }
                ))
                .aria_selected(selected)
                .aria_level(span.depth as usize + 1)
                .when(has_children, |row| row.aria_expanded(expanded))
                .tab_index(0)
                .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                .min_w_0()
                .flex_1()
                .h_full()
                .flex()
                .when(compact, |row| {
                    row.flex_col().items_start().justify_center().gap_1()
                })
                .when(!compact, |row| row.items_center().justify_between())
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.focus_handle.focus(window, cx);
                    this.focus_full_trace_span(selected_span.clone(), cx)
                }))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(span.name.clone())
                                .child(execution_tag(&role, execution_role_for_span(span))),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(metadata_tint)
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(operation),
                        ),
                )
                .child(
                    div()
                        .when(compact, |meta| meta.w_full())
                        .when(!compact, |meta| {
                            meta.w(px(FULL_TRACE_STATUS_RAIL)).flex_none().justify_end()
                        })
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .when(evidence, |meta| meta.child(tag("Evidence", Theme::AMBER)))
                        .child(tag(status, status_tint))
                        .child(div().text_xs().text_color(metadata_tint).child(format!(
                            "{:.1} ms{}",
                            span.duration_nano as f64 / 1_000_000.,
                            if has_children { " · children" } else { "" }
                        ))),
                ),
        )
}

pub(super) fn full_trace_timeline_row(
    span: &SpanRow,
    state: FullTraceRowState,
    trace_start: u64,
    trace_duration: u64,
    cx: &Context<FailureInbox>,
) -> gpui::Stateful<Div> {
    let FullTraceRowState {
        evidence,
        search_match,
        selected,
        compact,
        row_height,
    } = state;
    let offset =
        span.start_time_unix_nano.saturating_sub(trace_start) as f32 / trace_duration.max(1) as f32;
    let width = (span.duration_nano as f32 / trace_duration.max(1) as f32)
        .clamp(0.006, 1.0 - offset.min(0.994));
    let selected_span = span.clone();
    let role = full_trace_role(span);
    let (status, status_tint) = span_status(span);
    let metadata_tint = if selected { Theme::MUTED } else { Theme::DIM };
    div()
        .id(format!("timeline-span-{}", span.span_id))
        .role(Role::ListBoxOption)
        .aria_label(format!(
            "{}; role {}; status {}; {:.1} milliseconds{}",
            span.name,
            role,
            status,
            span.duration_nano as f64 / 1_000_000.,
            if evidence { "; evidence" } else { "" }
        ))
        .aria_selected(selected)
        .tab_index(0)
        .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
        .w_full()
        .h(px(row_height))
        .px_3()
        .flex()
        .when(compact, |row| {
            row.flex_col()
                .items_stretch()
                .justify_center()
                .gap_2()
                .py_2()
        })
        .when(!compact, |row| row.items_center().gap_3())
        .border_b_1()
        .border_color(Theme::BORDER)
        .bg(if selected {
            Theme::SELECTED
        } else if search_match {
            Theme::INFO_SURFACE
        } else {
            Theme::BG
        })
        .cursor_pointer()
        .hover(|style| style.bg(Theme::PANEL_ALT))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.focus_handle.focus(window, cx);
            this.focus_full_trace_span(selected_span.clone(), cx)
        }))
        .child(
            div()
                .when(compact, |label| label.w_full())
                .when(!compact, |label| {
                    label.w(px(FULL_TRACE_TIMELINE_LABEL)).flex_none()
                })
                .min_w_0()
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(span.name.clone()),
                        )
                        .child(
                            div()
                                .mt_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .text_color(metadata_tint)
                                .whitespace_nowrap()
                                .child(
                                    execution_tag(&role, execution_role_for_span(span))
                                        .max_w(px(FULL_TRACE_TIMELINE_ROLE_MAX))
                                        .overflow_hidden()
                                        .text_ellipsis(),
                                )
                                .child(div().text_color(status_tint).child(status))
                                .child(format!("{:.1} ms", span.duration_nano as f64 / 1_000_000.))
                                .when(evidence, |metadata| {
                                    metadata.child(tag("Evidence", Theme::AMBER))
                                }),
                        ),
                ),
        )
        .child(
            div()
                .relative()
                .h(px(24.))
                .flex_1()
                .rounded_sm()
                .bg(Theme::PANEL_ALT)
                .child(
                    div()
                        .absolute()
                        .left(relative(offset))
                        .top(px(5.))
                        .h(px(14.))
                        .w(relative(width))
                        .rounded_sm()
                        .bg(if evidence { Theme::AMBER } else { Theme::CYAN }),
                ),
        )
}

pub(super) fn full_trace_role(span: &SpanRow) -> String {
    for key in [
        "agent.role",
        "gen_ai.agent.role",
        "openinference.agent.role",
        "agent.name",
        "gen_ai.agent.name",
        "openinference.agent.name",
    ] {
        if let Some(value) = span.attributes.get(key).and_then(serde_json::Value::as_str) {
            return humanize(value);
        }
    }
    match span.category.as_str() {
        "llm" | "model" => "Model".into(),
        "tool" => "Tool".into(),
        "agent" => "Agent".into(),
        "chain" => "Chain".into(),
        value => humanize(value),
    }
}

pub(super) fn execution_role_for_span(span: &SpanRow) -> ExecutionRole {
    let role = full_trace_role(span).to_lowercase();
    if role.contains("planner") {
        ExecutionRole::Planner
    } else if role.contains("browser") {
        ExecutionRole::Browser
    } else if role.contains("verifier") || role.contains("verify") {
        ExecutionRole::Verifier
    } else {
        match span.category.as_str() {
            "llm" | "model" => ExecutionRole::Model,
            "tool" => ExecutionRole::Tool,
            _ => ExecutionRole::Tool,
        }
    }
}

fn full_trace_operation(span: &SpanRow) -> String {
    for key in [
        "gen_ai.operation.name",
        "agent.operation",
        "tool.operation",
        "operation.name",
    ] {
        if let Some(value) = span.attributes.get(key).and_then(serde_json::Value::as_str) {
            return humanize(value);
        }
    }
    humanize(&span.category)
}

fn span_status(span: &SpanRow) -> (&'static str, Rgba) {
    match span.status_code {
        2 => ("Error", Theme::RED),
        1 => ("OK", Theme::GREEN),
        _ => ("Unset", Theme::DIM),
    }
}

pub(super) fn humanize(value: &str) -> String {
    value
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn severity_color(severity: FindingSeverity) -> Rgba {
    match severity {
        FindingSeverity::Critical | FindingSeverity::High => Theme::RED,
        FindingSeverity::Medium => Theme::AMBER,
        FindingSeverity::Low => Theme::CYAN,
        FindingSeverity::Info => Theme::MUTED,
    }
}

#[cfg(test)]
mod trend_tests {
    use super::*;

    fn span_with_attributes(attributes: &[(&str, &str)]) -> SpanRow {
        SpanRow {
            logical_trace_id: "trace".into(),
            revision: 1,
            span_id: "span".into(),
            parent_span_id: None,
            name: "agent step".into(),
            category: "agent".into(),
            start_time_unix_nano: 0,
            duration_nano: 1,
            status_code: 1,
            status_message: String::new(),
            depth: 0,
            has_children: false,
            attributes: attributes
                .iter()
                .map(|(key, value)| ((*key).into(), serde_json::Value::String((*value).into())))
                .collect(),
            payload_refs: Default::default(),
            events: Vec::new(),
            links: Vec::new(),
        }
    }

    #[test]
    fn explicit_agent_role_wins_over_shared_agent_name() {
        let span = span_with_attributes(&[
            ("agent.name", "returns-support-agent"),
            ("agent.role", "browser"),
        ]);

        assert_eq!(full_trace_role(&span), "Browser");
        assert_eq!(execution_role_for_span(&span), ExecutionRole::Browser);
    }

    fn recurrence(rates: &[(u64, u64)]) -> FailureRecurrenceSeriesV1 {
        FailureRecurrenceSeriesV1 {
            started_at_unix_nano: 0,
            ended_at_unix_nano: rates.len() as u64,
            bucket_width_nano: 1,
            buckets: rates
                .iter()
                .enumerate()
                .map(|(index, (affected, eligible))| FailureRecurrenceBucketV1 {
                    started_at_unix_nano: index as u64,
                    ended_at_unix_nano: index as u64 + 1,
                    eligible_run_count: *eligible,
                    affected_run_count: *affected,
                    finding_count: *affected,
                    recurrence_rate_basis_points: (*eligible > 0)
                        .then(|| ((*affected * 10_000) / *eligible) as u16),
                })
                .collect(),
        }
    }

    #[test]
    fn trend_compares_first_and_last_observed_buckets() {
        assert_eq!(trend_summary(&[1, 0, 0, 1, 0, 0, 1]).0, "Intermittent");
        assert_eq!(trend_summary(&[1, 0, 0, 3]).0, "+200%");
        assert_eq!(trend_summary(&[4, 0, 0, 2]).0, "-50%");
        assert_eq!(trend_summary(&[]).0, "New");
    }

    #[test]
    fn recurrence_trend_compares_rates_instead_of_raw_run_counts() {
        let series = recurrence(&[(1, 1), (0, 0), (1, 4), (0, 0)]);
        assert_eq!(recurrence_trend_summary(&series).0, "-75%");
    }

    #[test]
    fn recurrence_trend_distinguishes_no_population_from_zero_failures() {
        let no_recent_runs = recurrence(&[(1, 1), (0, 0)]);
        assert_eq!(
            recurrence_trend_summary(&no_recent_runs).0,
            "No recent runs"
        );

        let resolved = recurrence(&[(1, 1), (0, 1)]);
        assert_eq!(recurrence_trend_summary(&resolved).0, "-100%");
    }
}
