mod compact_row;
mod filters;

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    AppContext, Context, Div, EventEmitter, FontWeight, IntoElement, Render, Role, Toggled, Window,
    div, prelude::*, px, uniform_list,
};
use perseval_service::{
    IdentityQualityV1, LiveTraceService, QueryScopeCriteriaV1, QueryScopeV1,
    RUN_COMPARISON_REQUEST_SCHEMA_VERSION, RunComparisonRequestV1, RunFiltersV1, RunSummary,
    TraceLifecycle,
};

use crate::components::{
    DataColumn, button, button_state, data_columns, data_page_header, data_page_toolbar,
    data_table_header, tag,
};
use crate::controllers::BoundedPageCache;
use crate::design::{Breakpoint, Theme};
use crate::workbench::{ProjectScope, QueryScope};

const RUN_PAGE_SIZE: u64 = 200;
const NANOS_PER_SECOND: u64 = 1_000_000_000;
const RUN_COLUMNS_WIDE: [DataColumn; 7] = [
    DataColumn::Flexible,
    DataColumn::Fixed(116.),
    DataColumn::Fixed(116.),
    DataColumn::Fixed(108.),
    DataColumn::Fixed(92.),
    DataColumn::Fixed(92.),
    DataColumn::Fixed(112.),
];
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum RunTimeWindow {
    #[default]
    All,
    LastHour,
    LastDay,
    LastWeek,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunsFilterMenu {
    Environment,
    Build,
    Session,
    Lifecycle,
    Identity,
    Started,
}

impl RunTimeWindow {
    fn label(self) -> &'static str {
        match self {
            Self::All => "All time",
            Self::LastHour => "Last hour",
            Self::LastDay => "Last 24 hours",
            Self::LastWeek => "Last 7 days",
        }
    }

    fn duration_nanos(self) -> Option<u64> {
        match self {
            Self::All => None,
            Self::LastHour => Some(60 * 60 * NANOS_PER_SECOND),
            Self::LastDay => Some(24 * 60 * 60 * NANOS_PER_SECOND),
            Self::LastWeek => Some(7 * 24 * 60 * 60 * NANOS_PER_SECOND),
        }
    }

    fn scope_key(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::LastHour => Some("last_hour"),
            Self::LastDay => Some("last_day"),
            Self::LastWeek => Some("last_week"),
        }
    }

    fn from_scope(value: Option<&str>) -> Self {
        match value {
            Some("last_hour") => Self::LastHour,
            Some("last_day") => Self::LastDay,
            Some("last_week") => Self::LastWeek,
            _ => Self::All,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum RunsEvent {
    OpenTrace {
        project_id: String,
        logical_trace_id: String,
        revision: u64,
    },
    OpenCompare(RunComparisonRequestV1),
    ScopeChanged(QueryScope),
}

pub(crate) struct RunsScreen {
    service: Arc<LiveTraceService>,
    filters: RunFiltersV1,
    pages: BoundedPageCache<u64, Vec<RunSummary>>,
    loading_pages: HashSet<u64>,
    total_runs: u64,
    loading: bool,
    error: Option<String>,
    request_generation: u64,
    selected_runs: Vec<RunSummary>,
    selection_error: Option<String>,
    time_window: RunTimeWindow,
    open_filter_menu: Option<RunsFilterMenu>,
    environment_options: Vec<String>,
    build_options: Vec<String>,
    session_options: Vec<String>,
    text_scale: f32,
}

impl EventEmitter<RunsEvent> for RunsScreen {}

impl RunsScreen {
    pub(crate) fn new(
        service: Arc<LiveTraceService>,
        project_id: Option<String>,
        cached_pages: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut screen = Self {
            service,
            filters: RunFiltersV1 {
                scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                    project_id,
                    ..QueryScopeCriteriaV1::default()
                }),
                ..RunFiltersV1::default()
            },
            pages: BoundedPageCache::new(cached_pages),
            loading_pages: HashSet::new(),
            total_runs: 0,
            loading: false,
            error: None,
            request_generation: 0,
            selected_runs: Vec::new(),
            selection_error: None,
            time_window: RunTimeWindow::All,
            open_filter_menu: None,
            environment_options: Vec::new(),
            build_options: Vec::new(),
            session_options: Vec::new(),
            text_scale: 1.,
        };
        screen.reload(cx);
        screen
    }

    pub(crate) fn set_text_scale(&mut self, text_scale: f32, cx: &mut Context<Self>) {
        self.text_scale = text_scale.clamp(1., 2.);
        cx.notify();
    }

    pub(crate) fn set_query_scope(&mut self, scope: &QueryScope, cx: &mut Context<Self>) {
        let project_id = match &scope.project {
            ProjectScope::Project(project_id) => Some(project_id.clone()),
            ProjectScope::AllProjects => None,
        };
        let time_window = RunTimeWindow::from_scope(scope.time_range.as_deref());
        let next_filters = RunFiltersV1 {
            scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                project_id,
                environment: scope.environment.clone(),
                build_id: scope.build.clone(),
                session_id: scope.session.clone(),
                started_after_unix_nano: scope.started_after_unix_nano(unix_time_nanos()),
                ..QueryScopeCriteriaV1::default()
            }),
            ..RunFiltersV1::default()
        };
        if self.filters == next_filters && self.time_window == time_window {
            return;
        }
        self.filters = next_filters;
        self.time_window = time_window;
        self.open_filter_menu = None;
        self.environment_options.clear();
        self.build_options.clear();
        self.session_options.clear();
        self.selected_runs.clear();
        self.selection_error = None;
        self.reload(cx);
    }

    fn query_scope(&self) -> QueryScope {
        QueryScope {
            project: self
                .filters
                .scope
                .criteria
                .project_id
                .clone()
                .map(ProjectScope::Project)
                .unwrap_or(ProjectScope::AllProjects),
            environment: self.filters.scope.criteria.environment.clone(),
            build: self.filters.scope.criteria.build_id.clone(),
            session: self.filters.scope.criteria.session_id.clone(),
            time_range: self.time_window.scope_key().map(str::to_owned),
        }
    }

    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        self.request_generation = self.request_generation.wrapping_add(1);
        let generation = self.request_generation;
        self.loading = true;
        self.error = None;
        self.pages.clear();
        self.loading_pages.clear();
        let service = self.service.clone();
        let filters = self.filters.clone();
        let task = cx.background_spawn(async move {
            let total = service.run_count_filtered(&filters)?;
            let first_page = service.list_runs_filtered(&filters, 0, RUN_PAGE_SIZE as u32)?;
            Ok::<_, perseval_service::LiveServiceError>((total, first_page))
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                if this.request_generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok((total, first_page)) => {
                        this.total_runs = total;
                        refresh_selected_runs(&mut this.selected_runs, &first_page);
                        merge_run_filter_options(
                            &first_page,
                            &mut this.environment_options,
                            &mut this.build_options,
                            &mut this.session_options,
                        );
                        this.pages.insert(0, first_page);
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn ensure_page(&mut self, index: usize, cx: &mut Context<Self>) {
        let page = index as u64 / RUN_PAGE_SIZE;
        if self.pages.contains_key(&page) || !self.loading_pages.insert(page) {
            return;
        }
        let service = self.service.clone();
        let filters = self.filters.clone();
        let generation = self.request_generation;
        let task = cx.background_spawn(async move {
            service.list_runs_filtered(&filters, page * RUN_PAGE_SIZE, RUN_PAGE_SIZE as u32)
        });
        cx.spawn(async move |weak, cx| {
            let result = task.await;
            let _ = weak.update(cx, |this, cx| {
                this.loading_pages.remove(&page);
                if this.request_generation != generation {
                    return;
                }
                match result {
                    Ok(runs) => {
                        merge_run_filter_options(
                            &runs,
                            &mut this.environment_options,
                            &mut this.build_options,
                            &mut this.session_options,
                        );
                        this.pages.insert(page, runs);
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn run_at(&self, index: usize) -> Option<&RunSummary> {
        let page = index as u64 / RUN_PAGE_SIZE;
        self.pages.peek(&page)?.get(index % RUN_PAGE_SIZE as usize)
    }

    fn open_run(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(run) = self.run_at(index) else {
            return;
        };
        cx.emit(RunsEvent::OpenTrace {
            project_id: run.project_id.clone(),
            logical_trace_id: run.logical_trace_id.clone(),
            revision: run.revision,
        });
    }

    fn toggle_run_selection(&mut self, run: RunSummary, cx: &mut Context<Self>) {
        self.selection_error = None;
        if let Some(index) = self
            .selected_runs
            .iter()
            .position(|selected| selected.logical_trace_id == run.logical_trace_id)
        {
            self.selected_runs.remove(index);
        } else if self.filters.scope.criteria.project_id.is_none() {
            self.selection_error = Some(
                "Choose one project before comparing runs; All Projects never merges identities."
                    .into(),
            );
        } else if self.selected_runs.len() == 2 {
            self.selection_error =
                Some("Compare accepts exactly two runs. Clear one selection first.".into());
        } else {
            self.selected_runs.push(run);
        }
        cx.notify();
    }

    fn open_compare(&mut self, cx: &mut Context<Self>) {
        match comparison_request(&self.filters.scope, &self.selected_runs) {
            Ok(request) => cx.emit(RunsEvent::OpenCompare(request)),
            Err(error) => self.selection_error = Some(error),
        }
        cx.notify();
    }

    fn toggle_filter_menu(&mut self, menu: RunsFilterMenu, cx: &mut Context<Self>) {
        self.open_filter_menu = (self.open_filter_menu != Some(menu)).then_some(menu);
        cx.notify();
    }

    fn select_text_filter(
        &mut self,
        menu: RunsFilterMenu,
        value: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let mut criteria = self.filters.scope.criteria.clone();
        match menu {
            RunsFilterMenu::Environment => criteria.environment = value,
            RunsFilterMenu::Build => criteria.build_id = value,
            RunsFilterMenu::Session => criteria.session_id = value,
            _ => return,
        }
        self.filters.scope = QueryScopeV1::new(criteria);
        self.open_filter_menu = None;
        self.selected_runs.clear();
        self.selection_error = None;
        self.reload(cx);
        cx.emit(RunsEvent::ScopeChanged(self.query_scope()));
    }

    fn select_lifecycle(&mut self, value: Option<TraceLifecycle>, cx: &mut Context<Self>) {
        self.filters.lifecycle = value;
        self.open_filter_menu = None;
        self.selected_runs.clear();
        self.selection_error = None;
        self.reload(cx);
    }

    fn select_identity(&mut self, value: Option<IdentityQualityV1>, cx: &mut Context<Self>) {
        self.filters.identity_quality = value;
        self.open_filter_menu = None;
        self.selected_runs.clear();
        self.selection_error = None;
        self.reload(cx);
    }

    fn select_time_window(&mut self, value: RunTimeWindow, cx: &mut Context<Self>) {
        self.time_window = value;
        let now = unix_time_nanos();
        let mut criteria = self.filters.scope.criteria.clone();
        criteria.started_after_unix_nano = self
            .time_window
            .duration_nanos()
            .map(|duration| now.saturating_sub(duration));
        criteria.started_before_unix_nano = None;
        self.filters.scope = QueryScopeV1::new(criteria);
        self.open_filter_menu = None;
        self.selected_runs.clear();
        self.selection_error = None;
        self.reload(cx);
        cx.emit(RunsEvent::ScopeChanged(self.query_scope()));
    }

    fn clear_filters(&mut self, cx: &mut Context<Self>) {
        let project_id = self.filters.scope.criteria.project_id.clone();
        self.filters = RunFiltersV1 {
            scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                project_id,
                ..QueryScopeCriteriaV1::default()
            }),
            ..RunFiltersV1::default()
        };
        self.time_window = RunTimeWindow::All;
        self.open_filter_menu = None;
        self.selected_runs.clear();
        self.selection_error = None;
        self.reload(cx);
        cx.emit(RunsEvent::ScopeChanged(self.query_scope()));
    }

    fn render_row(
        &self,
        run: &RunSummary,
        index: usize,
        compact: bool,
        compact_row_height: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if compact {
            return self.render_compact_row(run, index, compact_row_height, cx);
        }
        let status_tint = if run.error_count > 0 {
            Theme::RED
        } else if run.finding_count > 0 {
            Theme::AMBER
        } else {
            Theme::GREEN
        };
        let selected = self
            .selected_runs
            .iter()
            .any(|candidate| candidate.logical_trace_id == run.logical_trace_id);
        let selected_run = run.clone();
        let mut cells = vec![
            div()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(run.title.clone()),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(Theme::DIM)
                        .child(short_id(&run.logical_trace_id).to_owned()),
                )
                .into_any_element(),
        ];
        if !compact {
            cells.extend([
                value_or_unknown(run.session_id.as_deref()).into_any_element(),
                value_or_unknown(run.build_id.as_deref()).into_any_element(),
                value_or_unknown(run.environment.as_deref()).into_any_element(),
            ]);
        }
        cells.extend([
            tag(
                identity_label(run.identity_quality),
                identity_tint(run.identity_quality),
            )
            .into_any_element(),
            tag(
                if run.error_count > 0 {
                    "ERRORS"
                } else if run.finding_count > 0 {
                    "FINDINGS"
                } else {
                    "CLEAN"
                },
                status_tint,
            )
            .into_any_element(),
            div()
                .text_xs()
                .text_color(Theme::MUTED)
                .child(format!("{} spans · r{}", run.span_count, run.revision))
                .into_any_element(),
        ]);
        div()
            .id(("run-row", index))
            .role(Role::Row)
            .aria_label(format!(
                "{}; {}; session {}; build {}; environment {}; {} spans; {} findings; {} errors",
                run.title,
                lifecycle_label(run.lifecycle),
                run.session_id.as_deref().unwrap_or("Unknown"),
                run.build_id.as_deref().unwrap_or("Unknown"),
                run.environment.as_deref().unwrap_or("Unknown"),
                run.span_count,
                run.finding_count,
                run.error_count
            ))
            .w_full()
            .h(px(64.))
            .px_6()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL)
            .child(
                div()
                    .id(("select-run", index))
                    .role(Role::CheckBox)
                    .aria_label(format!("Select {} for comparison", run.title))
                    .aria_toggled(if selected {
                        Toggled::True
                    } else {
                        Toggled::False
                    })
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .w(px(28.))
                    .h(px(28.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(if selected { Theme::CYAN } else { Theme::BORDER })
                    .bg(if selected { Theme::SELECTED } else { Theme::BG })
                    .text_xs()
                    .text_color(Theme::CYAN)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_run_selection(selected_run.clone(), cx)
                    }))
                    .child(if selected { "✓" } else { "" }),
            )
            .child(
                div()
                    .id(("open-run", index))
                    .role(Role::Button)
                    .aria_label(format!("Open full trace for {}", run.title))
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .cursor_pointer()
                    .hover(|style| style.bg(Theme::PANEL_ALT))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_run(index, cx)))
                    .child(data_columns(&RUN_COLUMNS_WIDE, cells)),
            )
            .into_any_element()
    }
}

impl Render for RunsScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width: f32 = window.viewport_size().width.into();
        let compact = runs_breakpoint(width, self.text_scale) == Breakpoint::Compact;
        let compact_row_height = 112. * self.text_scale;
        let total = self.total_runs as usize;
        let list = uniform_list(
            "runs-browser-list",
            total,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                range
                    .map(|index| {
                        this.ensure_page(index, cx);
                        this.run_at(index)
                            .map(|run| this.render_row(run, index, compact, compact_row_height, cx))
                            .unwrap_or_else(|| {
                                div()
                                    .id(("run-placeholder", index))
                                    .w_full()
                                    .h(px(if compact { compact_row_height } else { 64. }))
                                    .border_b_1()
                                    .border_color(Theme::BORDER)
                                    .bg(Theme::PANEL_ALT)
                                    .into_any_element()
                            })
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .w_full()
        .flex_1()
        .min_h_0();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(Theme::BG)
            .child(
                data_page_header(
                    "Runs",
                    format!(
                        "{} runs · grouped by explicit session and build identity when available",
                        self.total_runs
                    ),
                    tag(
                        if self.filters.scope.criteria.project_id.is_some() {
                            "PROJECT SCOPE"
                        } else {
                            "ALL PROJECTS"
                        },
                        if self.filters.scope.criteria.project_id.is_some() {
                            Theme::CYAN
                        } else {
                            Theme::AMBER
                        },
                    ),
                )
                .child(
                    data_page_toolbar()
                        .child(
                            button(
                                &format!(
                                    "Environment: {}",
                                    self.filters
                                        .scope
                                        .criteria
                                        .environment
                                        .as_deref()
                                        .unwrap_or("All")
                                ),
                                self.filters.scope.criteria.environment.is_some(),
                            )
                            .id("runs-environment-filter")
                            .role(Role::Button)
                            .aria_label("Choose environment filter")
                            .aria_expanded(
                                self.open_filter_menu == Some(RunsFilterMenu::Environment),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_filter_menu(RunsFilterMenu::Environment, cx)
                            })),
                        )
                        .child(
                            button(
                                &format!(
                                    "Build: {}",
                                    self.filters
                                        .scope
                                        .criteria
                                        .build_id
                                        .as_deref()
                                        .unwrap_or("All")
                                ),
                                self.filters.scope.criteria.build_id.is_some(),
                            )
                            .id("runs-build-filter")
                            .role(Role::Button)
                            .aria_label("Choose build filter")
                            .aria_expanded(self.open_filter_menu == Some(RunsFilterMenu::Build))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_filter_menu(RunsFilterMenu::Build, cx)
                            })),
                        )
                        .child(
                            button(
                                &format!(
                                    "Session: {}",
                                    self.filters
                                        .scope
                                        .criteria
                                        .session_id
                                        .as_deref()
                                        .unwrap_or("All")
                                ),
                                self.filters.scope.criteria.session_id.is_some(),
                            )
                            .id("runs-session-filter")
                            .role(Role::Button)
                            .aria_label("Choose session filter")
                            .aria_expanded(self.open_filter_menu == Some(RunsFilterMenu::Session))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_filter_menu(RunsFilterMenu::Session, cx)
                            })),
                        )
                        .child(
                            button(
                                &format!(
                                    "Lifecycle: {}",
                                    self.filters.lifecycle.map(lifecycle_label).unwrap_or("All")
                                ),
                                self.filters.lifecycle.is_some(),
                            )
                            .id("runs-lifecycle-filter")
                            .role(Role::Button)
                            .aria_label("Choose lifecycle filter")
                            .aria_expanded(self.open_filter_menu == Some(RunsFilterMenu::Lifecycle))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_filter_menu(RunsFilterMenu::Lifecycle, cx)
                            })),
                        )
                        .child(
                            button(
                                &format!(
                                    "Identity: {}",
                                    self.filters
                                        .identity_quality
                                        .map(identity_filter_label)
                                        .unwrap_or("All")
                                ),
                                self.filters.identity_quality.is_some(),
                            )
                            .id("runs-identity-filter")
                            .role(Role::Button)
                            .aria_label("Choose identity-quality filter")
                            .aria_expanded(self.open_filter_menu == Some(RunsFilterMenu::Identity))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_filter_menu(RunsFilterMenu::Identity, cx)
                            })),
                        )
                        .child(
                            button(
                                &format!("Started: {}", self.time_window.label()),
                                self.time_window != RunTimeWindow::All,
                            )
                            .id("runs-time-filter")
                            .role(Role::Button)
                            .aria_label("Choose start-time filter")
                            .aria_expanded(self.open_filter_menu == Some(RunsFilterMenu::Started))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_filter_menu(RunsFilterMenu::Started, cx)
                            })),
                        )
                        .child(
                            button("Reset", false)
                                .id("runs-reset-filters")
                                .role(Role::Button)
                                .aria_label("Reset run filters")
                                .on_click(cx.listener(|this, _, _, cx| this.clear_filters(cx))),
                        ),
                )
                .when_some(self.render_open_filter_menu(cx), |header, menu| {
                    header.child(menu)
                }),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .mx_5()
                        .mt_3()
                        .p_3()
                        .rounded_sm()
                        .bg(Theme::DANGER_SURFACE)
                        .text_sm()
                        .text_color(Theme::RED)
                        .child(error),
                )
            })
            .when_some(self.selection_error.clone(), |view, error| {
                view.child(
                    div()
                        .mx_5()
                        .mt_3()
                        .p_3()
                        .rounded_sm()
                        .bg(Theme::WARNING_SURFACE)
                        .text_sm()
                        .text_color(Theme::AMBER)
                        .child(error),
                )
            })
            .when(!self.selected_runs.is_empty(), |view| {
                let ready = self.selected_runs.len() == 2
                    && self
                        .selected_runs
                        .iter()
                        .all(|run| matches!(run.lifecycle, TraceLifecycle::Finalized));
                let baseline = comparison_run_label(&self.selected_runs[0]);
                let orientation = self.selected_runs.get(1).map_or_else(
                    || format!("Baseline: {baseline} · select a candidate"),
                    |candidate| {
                        format!(
                            "Baseline: {baseline}  →  Candidate: {}",
                            comparison_run_label(candidate)
                        )
                    },
                );
                view.child(
                    div()
                        .h(px(48.))
                        .px_4()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(Theme::BORDER)
                        .bg(Theme::INFO_SURFACE)
                        .child(
                            div()
                                .min_w_0()
                                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(
                                    format!("{} of 2 runs selected", self.selected_runs.len()),
                                ))
                                .child(div().text_xs().text_color(Theme::MUTED).child(orientation)),
                        )
                        .child(
                            button_state("Compare selected", true, ready)
                                .id("compare-selected-runs")
                                .role(Role::Button)
                                .aria_label(if ready {
                                    "Compare the two selected finalized runs"
                                } else {
                                    "Comparison requires two finalized runs"
                                })
                                .when(ready, |button| {
                                    button.on_click(
                                        cx.listener(|this, _, _, cx| this.open_compare(cx)),
                                    )
                                }),
                        ),
                )
            })
            .when(!compact, |view| {
                view.child(data_table_header(
                    28.,
                    data_columns(
                        &RUN_COLUMNS_WIDE,
                        vec![
                            "Run",
                            "Session",
                            "Build",
                            "Environment",
                            "Identity",
                            "Status",
                            "Size",
                        ]
                        .into_iter()
                        .map(|label| div().child(label).into_any_element())
                        .collect(),
                    ),
                ))
            })
            .child(if self.loading && total == 0 {
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(Theme::MUTED)
                    .child("Loading runs…")
            } else if total == 0 {
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(Theme::MUTED)
                    .child("No runs match this scope. Reset filters or send a trace.")
            } else {
                div().flex_1().min_h_0().flex().flex_col().child(list)
            })
    }
}

fn unix_time_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u64::MAX as u128) as u64
}

fn runs_breakpoint(width: f32, text_scale: f32) -> Breakpoint {
    Breakpoint::for_width(width / text_scale.clamp(1., 2.))
}

fn lifecycle_label(lifecycle: TraceLifecycle) -> &'static str {
    match lifecycle {
        TraceLifecycle::Live => "Live",
        TraceLifecycle::Quiescent => "Quiescent",
        TraceLifecycle::Finalized => "Finalized",
        TraceLifecycle::Reopened => "Reopened",
    }
}

fn identity_filter_label(quality: IdentityQualityV1) -> &'static str {
    match quality {
        IdentityQualityV1::Explicit => "Explicit",
        IdentityQualityV1::Inferred => "Inferred",
        IdentityQualityV1::Unknown => "Unknown",
    }
}

fn merge_run_filter_options(
    runs: &[RunSummary],
    environments: &mut Vec<String>,
    builds: &mut Vec<String>,
    sessions: &mut Vec<String>,
) {
    for run in runs {
        if let Some(value) = &run.environment {
            environments.push(value.clone());
        }
        if let Some(value) = &run.build_id {
            builds.push(value.clone());
        }
        if let Some(value) = &run.session_id {
            sessions.push(value.clone());
        }
    }
    for values in [environments, builds, sessions] {
        values.sort();
        values.dedup();
    }
}

fn value_or_unknown(value: Option<&str>) -> Div {
    div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_xs()
        .text_color(if value.is_some() {
            Theme::TEXT
        } else {
            Theme::DIM
        })
        .child(value.unwrap_or("Unknown").to_owned())
}

fn identity_label(quality: IdentityQualityV1) -> &'static str {
    match quality {
        IdentityQualityV1::Explicit => "EXPLICIT",
        IdentityQualityV1::Inferred => "INFERRED",
        IdentityQualityV1::Unknown => "UNKNOWN",
    }
}

fn identity_tint(quality: IdentityQualityV1) -> gpui::Rgba {
    match quality {
        IdentityQualityV1::Explicit => Theme::GREEN,
        IdentityQualityV1::Inferred => Theme::AMBER,
        IdentityQualityV1::Unknown => Theme::DIM,
    }
}

fn short_id(value: &str) -> &str {
    value.get(..value.len().min(16)).unwrap_or(value)
}

fn comparison_run_label(run: &RunSummary) -> String {
    run.build_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| short_id(&run.logical_trace_id))
        .to_string()
}

fn refresh_selected_runs(selected: &mut [RunSummary], visible: &[RunSummary]) {
    for selected_run in selected {
        if let Some(updated) = visible.iter().find(|run| {
            run.project_id == selected_run.project_id
                && run.logical_trace_id == selected_run.logical_trace_id
                && run.revision == selected_run.revision
        }) {
            *selected_run = updated.clone();
        }
    }
}

fn comparison_request(
    scope: &QueryScopeV1,
    selected_runs: &[RunSummary],
) -> Result<RunComparisonRequestV1, String> {
    scope.validate()?;
    let Some(project_id) = scope.criteria.project_id.as_deref() else {
        return Err(
            "Choose one project before comparing runs; All Projects never merges identities."
                .into(),
        );
    };
    let [baseline, candidate] = selected_runs else {
        return Err("Select exactly two runs to compare.".into());
    };
    if baseline.project_id != project_id || candidate.project_id != project_id {
        return Err(
            "Both runs must belong to the active project. Switch project scope and reselect."
                .into(),
        );
    }
    if !matches!(baseline.lifecycle, TraceLifecycle::Finalized)
        || !matches!(candidate.lifecycle, TraceLifecycle::Finalized)
    {
        return Err(
            "Both run revisions must be finalized before their execution shapes can be compared."
                .into(),
        );
    }
    Ok(RunComparisonRequestV1 {
        schema_version: RUN_COMPARISON_REQUEST_SCHEMA_VERSION.into(),
        scope: scope.clone(),
        baseline_trace_id: baseline.logical_trace_id.clone(),
        baseline_revision: baseline.revision,
        candidate_trace_id: candidate.logical_trace_id.clone(),
        candidate_revision: candidate.revision,
    })
}

#[cfg(test)]
mod tests;
