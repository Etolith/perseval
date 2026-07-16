mod components;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::time::Instant;

use gpui::{
    Context, Div, FontWeight, IntoElement, Render, Rgba, Role, Window, div, prelude::*, px,
    relative, uniform_list,
};
use perseval_service::{SpanCategory, SpanView, TRACE_FILE_ENV, TraceCatalog, TraceView};

use crate::design::Theme;
use components::*;

const SLOW_THRESHOLD_ENV: &str = "PERSEVAL_SLOW_THRESHOLD_MS";
const DEFAULT_SLOW_THRESHOLD_MS: u64 = 1_000;

#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkbenchSettings {
    slow_threshold_ms: u64,
}

impl WorkbenchSettings {
    pub(crate) fn from_environment() -> Self {
        let slow_threshold_ms = std::env::var(SLOW_THRESHOLD_ENV)
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_SLOW_THRESHOLD_MS);
        Self { slow_threshold_ms }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Errors,
    Slow,
    Tools,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InspectorTab {
    Evidence,
    Attributes,
    Raw,
}

pub(crate) struct Workbench {
    traces: Vec<TraceView>,
    configured_path: Option<String>,
    load_error: Option<String>,
    selected_trace: usize,
    selected_span: usize,
    expanded: HashSet<String>,
    filter: Filter,
    inspector_tab: InspectorTab,
    settings: WorkbenchSettings,
    profile_started: Option<Instant>,
}

impl Workbench {
    pub(crate) fn new(
        catalog: perseval_service::analysis::Result<TraceCatalog>,
        settings: WorkbenchSettings,
        profile_started: Option<Instant>,
    ) -> Self {
        let (traces, configured_path, load_error) = match catalog {
            Ok(catalog) => (
                catalog.traces,
                catalog
                    .configured_path
                    .map(|path| path.display().to_string()),
                None,
            ),
            Err(error) => (Vec::new(), None, Some(error.to_string())),
        };
        let expanded = traces
            .first()
            .map(|trace| trace.spans.iter().map(|span| span.id.clone()).collect())
            .unwrap_or_default();
        Self {
            traces,
            configured_path,
            load_error,
            selected_trace: 0,
            selected_span: 0,
            expanded,
            filter: Filter::All,
            inspector_tab: InspectorTab::Evidence,
            settings,
            profile_started,
        }
    }

    fn current_trace(&self) -> Option<&TraceView> {
        self.traces.get(self.selected_trace)
    }

    fn current_span(&self) -> Option<&SpanView> {
        self.current_trace()?.spans.get(self.selected_span)
    }

    fn select_trace(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected_trace = index;
        self.selected_span = 0;
        self.expanded = self
            .current_trace()
            .map(|trace| trace.spans.iter().map(|span| span.id.clone()).collect())
            .unwrap_or_default();
        self.filter = Filter::All;
        cx.notify();
    }

    fn select_span(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected_span = index;
        cx.notify();
    }

    fn toggle_span(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        cx.notify();
    }

    fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = filter;
        let visible = self.visible_spans();
        if !visible
            .iter()
            .any(|(index, _)| *index == self.selected_span)
            && let Some((index, _)) = visible.first()
        {
            self.selected_span = *index;
        }
        cx.notify();
    }

    fn visible_spans(&self) -> Vec<(usize, usize)> {
        let Some(trace) = self.current_trace() else {
            return Vec::new();
        };
        let topology = span_layout(&trace.spans, &self.expanded);
        trace
            .spans
            .iter()
            .enumerate()
            .filter_map(|(index, span)| {
                let (depth, ancestors_open) = topology[index];
                let matches = match self.filter {
                    Filter::All => true,
                    Filter::Errors => span.error.is_some(),
                    Filter::Slow => span.duration_ms > self.settings.slow_threshold_ms,
                    Filter::Tools => span.category == SpanCategory::Tool,
                };
                (ancestors_open && matches).then_some((index, depth))
            })
            .collect()
    }

    fn render_header(&self) -> Div {
        let span_count: usize = self.traces.iter().map(|trace| trace.spans.len()).sum();
        div()
            .h(px(58.))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px_5()
            .border_b_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().size_2().rounded_full().bg(Theme::CYAN))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .text_color(Theme::TEXT)
                            .child("PERSEVAL"),
                    )
                    .child(tag("LOCAL WORKSPACE", Theme::MUTED)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .text_xs()
                    .text_color(Theme::MUTED)
                    .child(format!(
                        "{} {}  ·  {} {}",
                        self.traces.len(),
                        plural(self.traces.len(), "run", "runs"),
                        span_count,
                        plural(span_count, "span", "spans")
                    ))
                    .child(div().size_1().rounded_full().bg(Theme::GREEN))
                    .child("Service embedded  ·  endpoint off"),
            )
    }

    fn render_empty(&self) -> Div {
        let (title, detail, color) = if let Some(error) = &self.load_error {
            ("Trace file could not be loaded", error.clone(), Theme::RED)
        } else {
            (
                "No trace source configured",
                format!(
                    "Set {TRACE_FILE_ENV} to a canonical Trace JSONL file and relaunch Perseval."
                ),
                Theme::CYAN,
            )
        };
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .bg(Theme::BG)
            .child(
                div()
                    .w(relative(0.55))
                    .max_w(px(680.))
                    .p_6()
                    .rounded_md()
                    .border_1()
                    .border_color(Theme::BORDER)
                    .bg(Theme::PANEL)
                    .child(div().size_2().rounded_full().bg(color))
                    .child(
                        div()
                            .mt_4()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(Theme::TEXT)
                            .child(title),
                    )
                    .child(div().mt_2().text_sm().text_color(Theme::MUTED).child(detail))
                    .child(
                        div()
                            .mt_5()
                            .p_3()
                            .rounded_sm()
                            .bg(Theme::BG)
                            .text_xs()
                            .text_color(Theme::TEXT)
                            .child(format!("{TRACE_FILE_ENV}=/path/to/traces.jsonl cargo run -p perseval-app --bin perseval")),
                    ),
            )
    }

    fn render_run_list(&self, cx: &mut Context<Self>) -> Div {
        let run_count = self.traces.len();
        let runs = uniform_list(
            "run-list",
            run_count,
            cx.processor(|this, range: Range<usize>, _, cx| {
                range
                    .filter_map(|index| {
                        let trace = this.traces.get(index)?;
                        Some(run_row(trace, index, index == this.selected_trace, cx))
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1();
        let sources = self
            .traces
            .iter()
            .filter_map(|trace| trace.source.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(" · ");
        div()
            .w(relative(0.20))
            .min_w(px(240.))
            .max_w(px(360.))
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL)
            .child(
                div()
                    .flex_none()
                    .p_4()
                    .border_b_1()
                    .border_color(Theme::BORDER)
                    .child(section_heading("RUN INBOX"))
                    .child(
                        div()
                            .mt_2()
                            .text_xs()
                            .text_color(Theme::DIM)
                            .child(format!("{} loaded · newest first", self.traces.len())),
                    ),
            )
            .child(runs)
            .child(
                div()
                    .flex_none()
                    .p_3()
                    .border_t_1()
                    .border_color(Theme::BORDER)
                    .text_xs()
                    .text_color(Theme::DIM)
                    .child(if sources.is_empty() {
                        self.configured_path
                            .clone()
                            .unwrap_or_else(|| "No sources".to_string())
                    } else {
                        format!("Sources  {sources}")
                    }),
            )
    }

    fn render_trace(&self, cx: &mut Context<Self>) -> Div {
        let trace = self.current_trace().expect("trace view requires a trace");
        let visible = self.visible_spans();
        let visible_count = visible.len();
        let rows = uniform_list(
            "span-list",
            visible_count,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                let Some(trace) = this.current_trace() else {
                    return Vec::new();
                };
                range
                    .filter_map(|position| {
                        let (index, depth) = *visible.get(position)?;
                        let span = trace.spans.get(index)?;
                        Some(span_row(
                            trace,
                            span,
                            index,
                            depth,
                            index == this.selected_span,
                            this.expanded.contains(&span.id),
                            this.settings.slow_threshold_ms,
                            cx,
                        ))
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .px_3();

        let cluster = trace.cluster.clone();
        div()
            .min_w_0()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .bg(Theme::BG)
            .child(
                div()
                    .flex_none()
                    .p_4()
                    .border_b_1()
                    .border_color(Theme::BORDER)
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .justify_between()
                            .child(trace_heading(trace))
                            .when_some(cluster, |row, cluster| {
                                row.child(tag(&format!("SHAPE CLUSTER  {cluster}"), Theme::MUTED))
                            }),
                    )
                    .child(self.render_filters(cx)),
            )
            .child(
                div()
                    .h(px(32.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(Theme::BORDER)
                    .bg(Theme::PANEL)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(Theme::DIM)
                    .child(div().w(relative(0.44)).pl_4().child("SPAN / OPERATION"))
                    .child(div().w(px(90.)).text_right().pr_4().child("DURATION"))
                    .child(div().flex_1().child("TIMELINE")),
            )
            .child(rows)
    }

    fn render_filters(&self, cx: &mut Context<Self>) -> Div {
        let mut row = div().mt_4().flex().items_center().gap_2();
        for filter in [Filter::All, Filter::Errors, Filter::Slow, Filter::Tools] {
            let label = match filter {
                Filter::All => "All spans".to_string(),
                Filter::Errors => "Errors".to_string(),
                Filter::Slow => format!(
                    "Slow > {}",
                    format_duration(self.settings.slow_threshold_ms)
                ),
                Filter::Tools => "Tools".to_string(),
            };
            row = row.child(
                button(label, self.filter == filter)
                    .id(("filter", filter as usize))
                    .role(Role::Button)
                    .aria_label(format!("Filter by {filter:?}"))
                    .aria_selected(self.filter == filter)
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter, cx))),
            );
        }
        let all_expanded = self.current_trace().is_some_and(|trace| {
            trace
                .spans
                .iter()
                .all(|span| self.expanded.contains(&span.id))
        });
        row.child(div().flex_1()).child(
            button(
                if all_expanded {
                    "Collapse all"
                } else {
                    "Expand all"
                }
                .to_string(),
                false,
            )
            .id("expand-all")
            .role(Role::Button)
            .aria_label(if all_expanded {
                "Collapse all spans"
            } else {
                "Expand all spans"
            })
            .tab_index(0)
            .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
            .on_click(cx.listener(move |this, _, _, cx| {
                if all_expanded {
                    this.expanded.clear();
                } else {
                    this.expanded = this
                        .current_trace()
                        .map(|trace| trace.spans.iter().map(|span| span.id.clone()).collect())
                        .unwrap_or_default();
                }
                cx.notify();
            })),
        )
    }

    fn render_inspector(&self, cx: &mut Context<Self>) -> Div {
        let trace = self.current_trace().expect("inspector requires a trace");
        let span = self.current_span().expect("inspector requires a span");
        let mut body = div()
            .id("inspector-body")
            .flex_1()
            .overflow_y_scroll()
            .p_4();
        match self.inspector_tab {
            InspectorTab::Evidence => {
                body = body
                    .child(section_label("INPUT"))
                    .child(code_block(
                        span.input.as_deref().unwrap_or("No input captured"),
                    ))
                    .child(section_label("OUTPUT"))
                    .child(code_block(
                        span.output.as_deref().unwrap_or("No output captured"),
                    ));
                if let Some(signal) = trace.divergence.as_ref().or(span.error.as_ref()) {
                    body = body
                        .child(section_label("FAILURE SIGNAL"))
                        .child(alert_block(signal));
                }
            }
            InspectorTab::Attributes => {
                if span.attributes.is_empty() {
                    body = body.child(empty_note("No attributes captured"));
                } else {
                    for (key, value) in &span.attributes {
                        body = body.child(attribute_row(key, value));
                    }
                }
            }
            InspectorTab::Raw => {
                body = body.child(code_block(&raw_span(span)));
            }
        }

        div()
            .w(relative(0.27))
            .min_w(px(320.))
            .max_w(px(520.))
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL)
            .child(inspector_heading(span))
            .child(self.render_tabs(cx))
            .child(body)
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tabs = div()
            .id("inspector-tabs")
            .h(px(42.))
            .flex_none()
            .flex()
            .items_end()
            .px_3()
            .gap_4()
            .border_b_1()
            .border_color(Theme::BORDER);
        for (tab, label) in [
            (InspectorTab::Evidence, "Evidence"),
            (InspectorTab::Attributes, "Attributes"),
            (InspectorTab::Raw, "Raw"),
        ] {
            let active = tab == self.inspector_tab;
            tabs = tabs.child(
                div()
                    .id(label)
                    .role(Role::Tab)
                    .aria_label(label)
                    .aria_selected(active)
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .h(px(34.))
                    .px_1()
                    .border_b_2()
                    .border_color(if active {
                        Theme::CYAN.into()
                    } else {
                        gpui::transparent_black()
                    })
                    .text_xs()
                    .text_color(if active { Theme::TEXT } else { Theme::MUTED })
                    .cursor_pointer()
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.inspector_tab = tab;
                        cx.notify();
                    })),
            );
        }
        tabs
    }
}

impl Render for Workbench {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(started) = self.profile_started.take() {
            eprintln!(
                "perseval_profile first_render_ms={:.3}",
                started.elapsed().as_secs_f64() * 1_000.0
            );
        }
        let content = if self.traces.is_empty() {
            self.render_empty()
        } else {
            div()
                .min_h_0()
                .flex_1()
                .flex()
                .child(self.render_run_list(cx))
                .child(self.render_trace(cx))
                .child(self.render_inspector(cx))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(Theme::BG)
            .text_color(Theme::TEXT)
            .child(self.render_header())
            .child(content)
    }
}

fn span_layout(spans: &[SpanView], expanded: &HashSet<String>) -> Vec<(usize, bool)> {
    let indices = spans
        .iter()
        .enumerate()
        .map(|(index, span)| (span.id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut layout = vec![None; spans.len()];

    for start in 0..spans.len() {
        if layout[start].is_some() {
            continue;
        }
        let mut path = Vec::new();
        let mut positions = HashMap::new();
        let mut current = start;
        let base = loop {
            if let Some((depth, open)) = layout[current] {
                break Some((depth + 1, open && expanded.contains(&spans[current].id)));
            }
            if positions.insert(current, path.len()).is_some() {
                for index in path.drain(..) {
                    layout[index] = Some((0, true));
                }
                break None;
            }
            path.push(current);
            let Some(parent) = spans[current]
                .parent_id
                .as_deref()
                .and_then(|parent| indices.get(parent))
                .copied()
            else {
                break Some((0, true));
            };
            current = parent;
        };

        if let Some((mut depth, mut ancestors_open)) = base {
            for index in path.into_iter().rev() {
                layout[index] = Some((depth, ancestors_open));
                ancestors_open &= expanded.contains(&spans[index].id);
                depth += 1;
            }
        }
    }

    layout.into_iter().map(Option::unwrap_or_default).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PersevalApp;

    #[test]
    fn native_app_does_not_open_otlp_by_default() {
        let app = PersevalApp::new();
        assert!(app.runtime().capabilities().filesystem_watchers);
        assert!(!app.runtime().capabilities().otlp_listener);
    }

    #[test]
    fn empty_catalog_is_an_honest_empty_state() {
        let workbench = Workbench::new(
            Ok(TraceCatalog::default()),
            WorkbenchSettings {
                slow_threshold_ms: 500,
            },
            None,
        );
        assert!(workbench.traces.is_empty());
        assert!(workbench.current_trace().is_none());
    }

    #[test]
    fn span_layout_is_linear_and_cycle_safe() {
        let spans = vec![
            SpanView {
                id: "a".into(),
                parent_id: Some("b".into()),
                name: "a".into(),
                category: SpanCategory::Agent,
                start_ms: 0,
                duration_ms: 1,
                error: None,
                input: None,
                output: None,
                attributes: Vec::new(),
            },
            SpanView {
                id: "b".into(),
                parent_id: Some("a".into()),
                name: "b".into(),
                category: SpanCategory::Agent,
                start_ms: 0,
                duration_ms: 1,
                error: None,
                input: None,
                output: None,
                attributes: Vec::new(),
            },
        ];

        assert_eq!(
            span_layout(&spans, &HashSet::new()),
            vec![(0, true), (0, true)]
        );
    }
}
