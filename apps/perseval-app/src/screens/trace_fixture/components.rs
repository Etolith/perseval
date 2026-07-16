use super::*;

pub(super) fn category_label(category: SpanCategory) -> &'static str {
    match category {
        SpanCategory::Agent => "AGENT",
        SpanCategory::Llm => "LLM",
        SpanCategory::Tool => "TOOL",
        SpanCategory::Retrieval => "RETRIEVAL",
        SpanCategory::Other => "OTHER",
    }
}

pub(super) fn category_color(category: SpanCategory) -> Rgba {
    match category {
        SpanCategory::Agent => Theme::CYAN,
        SpanCategory::Llm => Theme::PURPLE,
        SpanCategory::Tool => Theme::AMBER,
        SpanCategory::Retrieval => Theme::GREEN,
        SpanCategory::Other => Theme::MUTED,
    }
}

pub(super) fn plural(count: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
}

pub(super) fn format_duration(duration_ms: u64) -> String {
    if duration_ms >= 1_000 {
        format!("{:.2}s", duration_ms as f64 / 1_000.)
    } else {
        format!("{duration_ms}ms")
    }
}

pub(super) fn trace_summary(trace: &TraceView) -> String {
    let mut parts = Vec::new();
    if let Some(environment) = &trace.environment {
        parts.push(environment.clone());
    }
    parts.push(format!("{} spans", trace.spans.len()));
    parts.push(format_duration(trace.duration_ms));
    parts.join("  ·  ")
}

pub(super) fn trace_metrics(trace: &TraceView) -> Vec<Div> {
    let mut values = Vec::new();
    if let Some(cost) = trace.cost {
        values.push(metric_pill(format!("${cost:.4}"), Theme::MUTED));
    }
    if let Some(score) = trace.score {
        values.push(metric_pill(
            format!("{score:.2} score"),
            if trace.failed() {
                Theme::RED
            } else {
                Theme::GREEN
            },
        ));
    }
    values
}

pub(super) fn run_row(
    trace: &TraceView,
    index: usize,
    selected: bool,
    cx: &Context<Workbench>,
) -> gpui::Stateful<Div> {
    div()
        .id(("trace", index))
        .role(Role::ListBoxOption)
        .aria_label(format!("{}; {}", trace.title, trace_summary(trace)))
        .aria_selected(selected)
        .tab_index(0)
        .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
        .h(px(96.))
        .p_4()
        .border_1()
        .border_color(if selected { Theme::CYAN } else { Theme::BORDER })
        .bg(if selected {
            Theme::SELECTED_SUBTLE
        } else {
            Theme::PANEL
        })
        .cursor_pointer()
        .hover(|style| style.bg(Theme::PANEL_ALT))
        .on_click(cx.listener(move |this, _, _, cx| this.select_trace(index, cx)))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().size_2().rounded_full().bg(if trace.failed() {
                            Theme::RED
                        } else {
                            Theme::GREEN
                        }))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(Theme::TEXT)
                                .child(trace.title.clone()),
                        ),
                )
                .when_some(trace.observed_at.clone(), |row, value| {
                    row.child(div().text_xs().text_color(Theme::DIM).child(value))
                }),
        )
        .child(
            div()
                .mt_2()
                .text_xs()
                .text_color(Theme::MUTED)
                .child(trace_summary(trace)),
        )
        .when(trace.cost.is_some() || trace.score.is_some(), |row| {
            row.child(div().mt_2().flex().gap_2().children(trace_metrics(trace)))
        })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn span_row(
    trace: &TraceView,
    span: &SpanView,
    index: usize,
    depth: usize,
    selected: bool,
    expanded: bool,
    slow_threshold_ms: u64,
    cx: &Context<Workbench>,
) -> gpui::Stateful<Div> {
    let total = trace.duration_ms.max(1) as f32;
    let has_children = trace
        .spans
        .iter()
        .any(|item| item.parent_id.as_deref() == Some(&span.id));
    let offset = (span.start_ms as f32 / total * 250.).min(245.);
    let width = (span.duration_ms as f32 / total * 250.).clamp(5., 250. - offset);
    let span_id = span.id.clone();

    div()
        .id(("span-row", index))
        .role(Role::TreeItem)
        .aria_label(format!(
            "{}; {}; {}",
            span.name,
            category_label(span.category),
            format_duration(span.duration_ms)
        ))
        .aria_selected(selected)
        .aria_level(depth + 1)
        .when(has_children, |row| row.aria_expanded(expanded))
        .tab_index(0)
        .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
        .h(px(45.))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(Theme::BORDER)
        .bg(if selected { Theme::SELECTED } else { Theme::BG })
        .cursor_pointer()
        .hover(|style| style.bg(Theme::PANEL_ALT))
        .on_click(cx.listener(move |this, _, _, cx| this.select_span(index, cx)))
        .child(
            div()
                .w(relative(0.44))
                .min_w(px(240.))
                .flex()
                .items_center()
                .pl(px(10. + depth as f32 * 18.))
                .gap_2()
                .child(
                    div()
                        .id(("toggle", index))
                        .when(has_children, |toggle| {
                            toggle
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
                        .w_4()
                        .text_xs()
                        .text_color(if has_children {
                            Theme::MUTED
                        } else {
                            Theme::DIM
                        })
                        .child(if has_children {
                            if expanded { "▾" } else { "▸" }
                        } else {
                            "·"
                        })
                        .on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.toggle_span(span_id.clone(), cx)
                            }),
                        ),
                )
                .child(
                    div()
                        .size_2()
                        .rounded_sm()
                        .bg(category_color(span.category)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .text_sm()
                        .text_color(if selected { Theme::TEXT } else { Theme::MUTED })
                        .child(span.name.clone()),
                )
                .when(span.error.is_some(), |row| row.child(error_badge())),
        )
        .child(
            div()
                .w(px(90.))
                .min_w(px(90.))
                .text_right()
                .pr_4()
                .text_xs()
                .text_color(if span.duration_ms > slow_threshold_ms {
                    Theme::AMBER
                } else {
                    Theme::MUTED
                })
                .child(format_duration(span.duration_ms)),
        )
        .child(
            div()
                .h(px(18.))
                .flex_1()
                .min_w(px(180.))
                .flex()
                .items_center()
                .child(div().w(px(offset)))
                .child(
                    div()
                        .h(px(8.))
                        .w(px(width))
                        .rounded_sm()
                        .bg(if span.error.is_some() {
                            Theme::RED
                        } else {
                            category_color(span.category)
                        }),
                ),
        )
}

pub(super) fn trace_heading(trace: &TraceView) -> Div {
    div()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(Theme::TEXT)
                .child(trace.title.clone())
                .child(status_badge(trace.failed())),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(Theme::MUTED)
                .child(format!("{}  ·  {}", trace.id, trace_summary(trace))),
        )
}

pub(super) fn inspector_heading(span: &SpanView) -> Div {
    div()
        .p_4()
        .border_b_1()
        .border_color(Theme::BORDER)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .size_2()
                        .rounded_sm()
                        .bg(category_color(span.category)),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(Theme::TEXT)
                        .child(span.name.clone()),
                ),
        )
        .child(
            div()
                .mt_2()
                .flex()
                .gap_3()
                .text_xs()
                .text_color(Theme::MUTED)
                .child(category_label(span.category))
                .child(span.id.clone())
                .child(format_duration(span.duration_ms)),
        )
}

pub(super) fn button(label: String, active: bool) -> Div {
    div()
        .px_3()
        .py_1()
        .rounded_sm()
        .border_1()
        .border_color(if active { Theme::CYAN } else { Theme::BORDER })
        .bg(if active {
            Theme::ACCENT_MUTED
        } else {
            Theme::PANEL_ALT
        })
        .text_xs()
        .text_color(if active { Theme::TEXT } else { Theme::MUTED })
        .cursor_pointer()
        .hover(|style| style.border_color(Theme::CYAN).text_color(Theme::TEXT))
        .child(label)
}

pub(super) fn tag(label: &str, color: Rgba) -> Div {
    div()
        .px_2()
        .py(px(2.))
        .rounded_sm()
        .bg(Theme::PANEL_ALT)
        .text_xs()
        .text_color(color)
        .child(label.to_string())
}

pub(super) fn metric_pill(value: String, color: Rgba) -> Div {
    div()
        .px_2()
        .py(px(2.))
        .rounded_sm()
        .bg(Theme::PANEL_ALT)
        .text_xs()
        .text_color(color)
        .child(value)
}

pub(super) fn section_heading(label: &'static str) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(Theme::TEXT)
        .child(label)
}

pub(super) fn section_label(label: &'static str) -> Div {
    div()
        .mt_2()
        .mb_2()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(Theme::DIM)
        .child(label)
}

pub(super) fn code_block(content: &str) -> Div {
    div()
        .mb_4()
        .p_3()
        .rounded_sm()
        .border_1()
        .border_color(Theme::BORDER)
        .bg(Theme::BG)
        .text_xs()
        .text_color(Theme::MUTED)
        .child(content.to_string())
}

pub(super) fn alert_block(content: &str) -> Div {
    div()
        .p_3()
        .rounded_sm()
        .border_1()
        .border_color(Theme::RED)
        .bg(Theme::DANGER_SURFACE)
        .text_xs()
        .text_color(Theme::RED)
        .child(content.to_string())
}

pub(super) fn empty_note(content: &'static str) -> Div {
    div().p_3().text_xs().text_color(Theme::DIM).child(content)
}

pub(super) fn attribute_row(key: &str, value: &str) -> Div {
    div()
        .flex()
        .justify_between()
        .gap_4()
        .py_2()
        .border_b_1()
        .border_color(Theme::BORDER)
        .text_xs()
        .child(div().text_color(Theme::MUTED).child(key.to_string()))
        .child(
            div()
                .text_color(Theme::TEXT)
                .text_right()
                .child(value.to_string()),
        )
}

pub(super) fn status_badge(failed: bool) -> Div {
    div()
        .px_2()
        .rounded_sm()
        .bg(if failed {
            Theme::DANGER_SURFACE
        } else {
            Theme::SUCCESS_SURFACE
        })
        .text_xs()
        .text_color(if failed { Theme::RED } else { Theme::GREEN })
        .child(if failed { "FAILED" } else { "PASSED" })
}

pub(super) fn error_badge() -> Div {
    div()
        .px_2()
        .rounded_sm()
        .bg(Theme::DANGER_SURFACE)
        .text_xs()
        .text_color(Theme::RED)
        .child("ERROR")
}

pub(super) fn raw_span(span: &SpanView) -> String {
    format!(
        "{{\n  \"span_id\": \"{}\",\n  \"parent_id\": {:?},\n  \"kind\": \"{}\",\n  \"start_ms\": {},\n  \"duration_ms\": {},\n  \"status\": \"{}\"\n}}",
        span.id,
        span.parent_id,
        category_label(span.category),
        span.start_ms,
        span.duration_ms,
        if span.error.is_some() { "ERROR" } else { "OK" }
    )
}
