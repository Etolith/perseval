use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{Animation, AnimationExt, DragMoveEvent, Pixels, Point, ease_out_quint, prelude::*};

use super::*;
use crate::components::button;
use crate::workbench::FocusRegion;

#[derive(Clone)]
struct PaneResizeDrag {
    pane: PaneId,
    start_size: f32,
    start_offset: Arc<Mutex<Option<Point<Pixels>>>>,
}

struct PaneResizePreview;

impl Render for PaneResizePreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.))
    }
}

impl WorkbenchShell {
    pub(super) fn toggle_pane(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let visible = !self.pane_visible(pane);
        self.model
            .apply(WorkbenchAction::SetPaneVisible { pane, visible });
        if pane == PaneId::Inspector {
            self.model
                .apply(WorkbenchAction::SetInspectorAutoOpenSuppressed(!visible));
        }
        self.active_support_pane = visible.then_some(pane);
        if pane == PaneId::Inspector && self.active_editor_uses_embedded_inspector() {
            self.failure_inbox
                .update(cx, |inbox, cx| inbox.set_inspector_open(visible, cx));
        }
        self.persist();
        cx.notify();
    }

    pub(super) fn resize_pane_by(&mut self, pane: PaneId, delta: f32, cx: &mut Context<Self>) {
        self.resize_pane_to(pane, self.pane_size(pane) + delta, cx);
    }

    pub(super) fn reset_pane_size(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let defaults = crate::workbench::PaneLayout::default();
        let size = match pane {
            PaneId::PrimarySidebar => defaults.primary_sidebar_width,
            PaneId::Inspector => defaults.inspector_width,
            PaneId::BottomPanel => defaults.bottom_panel_height,
        };
        self.resize_pane_to(pane, size, cx);
    }

    fn resize_pane_to(&mut self, pane: PaneId, size: f32, cx: &mut Context<Self>) {
        self.model.apply(WorkbenchAction::ResizePane { pane, size });
        if pane == PaneId::Inspector {
            let width = self.model.state.panes.inspector_width;
            self.failure_inbox
                .update(cx, |inbox, cx| inbox.set_inspector_width(width, cx));
        }
        self.persist();
        cx.notify();
    }

    pub(super) fn focus_adjacent_pane(
        &mut self,
        reverse: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let region = adjacent_focus_region(
            self.model.state.focus,
            self.pane_visible(PaneId::PrimarySidebar),
            self.pane_visible(PaneId::Inspector),
            self.pane_visible(PaneId::BottomPanel),
            reverse,
        );
        self.model.apply(WorkbenchAction::SetFocus(region));
        match region {
            FocusRegion::PrimarySidebar => self.primary_sidebar_focus.focus(window, cx),
            FocusRegion::Inspector => self.inspector_pane_focus.focus(window, cx),
            FocusRegion::BottomPanel => self.bottom_panel_focus.focus(window, cx),
            _ => self.focus_handle.focus(window, cx),
        }
        self.persist();
        cx.notify();
    }

    pub(super) fn render_workspace_content(
        &self,
        editor: gpui::AnyElement,
        breakpoint: Breakpoint,
        active_kind: EditorKind,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let embedded_inspector = matches!(
            active_kind,
            EditorKind::FailureInvestigation | EditorKind::FullTrace
        );
        let effective = if breakpoint == Breakpoint::Wide {
            None
        } else {
            self.active_support_pane
        };
        let show_sidebar = self.pane_visible(PaneId::PrimarySidebar)
            && (breakpoint == Breakpoint::Wide || effective == Some(PaneId::PrimarySidebar));
        let show_bottom = self.pane_visible(PaneId::BottomPanel)
            && (breakpoint == Breakpoint::Wide || effective == Some(PaneId::BottomPanel));
        let show_context_inspector = self.pane_visible(PaneId::Inspector)
            && !embedded_inspector
            && (breakpoint == Breakpoint::Wide || effective == Some(PaneId::Inspector));

        if breakpoint == Breakpoint::Compact {
            if show_sidebar {
                return self.render_primary_sidebar(cx);
            }
            if show_bottom {
                return self.render_bottom_panel(cx);
            }
            if show_context_inspector {
                return self.render_context_inspector(cx);
            }
            return div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .size_full()
                .flex()
                .flex_col()
                .child(editor)
                .into_any_element();
        }

        let editor_column = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(editor),
            )
            .when(show_bottom, |column| {
                column
                    .child(self.render_resize_handle(PaneId::BottomPanel, false, cx))
                    .child(self.render_bottom_panel(cx))
            });

        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .when(show_sidebar, |row| {
                row.child(self.render_primary_sidebar(cx))
                    .child(self.render_resize_handle(PaneId::PrimarySidebar, true, cx))
            })
            .child(editor_column)
            .when(show_context_inspector, |row| {
                row.child(self.render_resize_handle(PaneId::Inspector, true, cx))
                    .child(self.render_context_inspector(cx))
            })
            .into_any_element()
    }

    pub(super) fn render_pane_controls(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        div()
            .relative()
            .child(
                button("View", self.view_menu_open)
                    .id("toggle-view-menu")
                    .role(Role::Button)
                    .aria_label(if compact {
                        "View and pane controls"
                    } else {
                        "View and pane controls; keyboard shortcuts available"
                    })
                    .aria_expanded(self.view_menu_open)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_view_menu(cx))),
            )
            .when(self.view_menu_open, |view| {
                view.child(deferred(
                    div()
                        .id("view-menu")
                        .role(Role::Menu)
                        .aria_label("View")
                        .absolute()
                        .top(px(36.))
                        .right_0()
                        .w(px(260.))
                        .p_2()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(Theme::BORDER)
                        .bg(Theme::PANEL)
                        .shadow_lg()
                        .child(
                            pane_menu_item(
                                "view-primary-sidebar",
                                "Primary sidebar",
                                "⌘B",
                                self.pane_visible(PaneId::PrimarySidebar),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_pane(PaneId::PrimarySidebar, cx);
                                this.view_menu_open = false;
                                cx.notify();
                            })),
                        )
                        .child(
                            pane_menu_item(
                                "view-inspector",
                                "Inspector",
                                "⌥⌘I",
                                self.pane_visible(PaneId::Inspector),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_pane(PaneId::Inspector, cx);
                                this.view_menu_open = false;
                                cx.notify();
                            })),
                        )
                        .child(
                            pane_menu_item(
                                "view-bottom-panel",
                                "Bottom panel",
                                "⌘J",
                                self.pane_visible(PaneId::BottomPanel),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_pane(PaneId::BottomPanel, cx);
                                this.view_menu_open = false;
                                cx.notify();
                            })),
                        ),
                ))
            })
    }

    fn render_primary_sidebar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut projects = div().flex().flex_col().gap_1();
        for (index, project) in self.projects.iter().enumerate() {
            let project_id = project.project_id.clone();
            projects = projects.child(
                div()
                    .id(("sidebar-project", index))
                    .role(Role::Button)
                    .aria_label(format!("Open project {}", project.display_name))
                    .aria_selected(self.selected_project_id() == Some(project.project_id.as_str()))
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .px_3()
                    .py_2()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(Theme::PANEL_ALT))
                    .child(project.display_name.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_project_scope(
                            crate::workbench::ProjectScope::Project(project_id.clone()),
                            cx,
                        )
                    })),
            );
        }
        let panel = div()
            .id("primary-sidebar")
            .role(Role::Navigation)
            .aria_label("Workspace and project sidebar")
            .track_focus(&self.primary_sidebar_focus)
            .tab_stop(true)
            .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
            .w(px(self.model.state.panes.primary_sidebar_width))
            .min_w(px(220.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL)
            .child(self.render_pane_header("Workspace", PaneId::PrimarySidebar, cx))
            .child(
                div()
                    .id("primary-sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(Theme::MUTED)
                            .child("Projects"),
                    )
                    .child(projects),
            );
        self.transition_panel("primary-sidebar-entry", panel)
    }

    fn render_context_inspector(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let scope = &self.model.state.scope;
        let details = format!(
            "Editor: {}\nProject: {}\nEnvironment: {}\nBuild: {}\nSession: {}\nTime: {}",
            active_title(&self.model),
            self.project_scope_label(),
            scope.environment.as_deref().unwrap_or("All"),
            scope.build.as_deref().unwrap_or("All"),
            scope.session.as_deref().unwrap_or("All"),
            scope.time_range.as_deref().unwrap_or("All time")
        );
        let panel = div()
            .id("workbench-inspector")
            .role(Role::Complementary)
            .aria_label(format!("Workbench context inspector. {details}"))
            .track_focus(&self.inspector_pane_focus)
            .tab_stop(true)
            .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
            .w(px(self.model.state.panes.inspector_width))
            .min_w(px(280.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL)
            .child(self.render_pane_header("Context", PaneId::Inspector, cx))
            .child(
                div()
                    .id("workbench-inspector-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .text_sm()
                    .text_color(Theme::MUTED)
                    .child(details),
            );
        self.transition_panel("context-inspector-entry", panel)
    }

    fn render_bottom_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let details = format!(
            "Endpoint {}\nQueue {} / {} batches\nJournal lag {}\nProjection lag {}\nAnalysis pending {}",
            self.endpoint,
            self.health.queue_batches,
            self.health.queue_batch_capacity,
            self.health.journal_lag,
            self.health.projection_lag,
            self.health.analysis_pending
        );
        let panel = div()
            .id("workbench-bottom-panel")
            .role(Role::Log)
            .aria_label(format!("Live system panel. {details}"))
            .track_focus(&self.bottom_panel_focus)
            .tab_stop(true)
            .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
            .h(px(self.model.state.panes.bottom_panel_height))
            .min_h(px(140.))
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL)
            .child(self.render_pane_header("Live system", PaneId::BottomPanel, cx))
            .child(
                div()
                    .id("workbench-bottom-panel-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .text_sm()
                    .text_color(Theme::MUTED)
                    .child(details),
            );
        self.transition_panel("bottom-panel-entry", panel)
    }

    fn transition_panel(&self, id: &'static str, panel: gpui::Stateful<Div>) -> gpui::AnyElement {
        if self.model.state.appearance.reduced_motion {
            panel.into_any_element()
        } else {
            panel
                .with_animation(
                    id,
                    Animation::new(Duration::from_millis(140)).with_easing(ease_out_quint()),
                    |panel, delta| panel.opacity(0.15 + 0.85 * delta),
                )
                .into_any_element()
        }
    }

    fn render_pane_header(&self, title: &str, pane: PaneId, cx: &mut Context<Self>) -> Div {
        div()
            .h(px(40.))
            .px_3()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(Theme::BORDER)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title.to_owned()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(self.pane_header_button("−", "Decrease pane size", pane, -24., cx))
                    .child(self.pane_header_button("↺", "Reset pane size", pane, 0., cx))
                    .child(self.pane_header_button("+", "Increase pane size", pane, 24., cx))
                    .child(
                        button("×", false)
                            .id(("close-pane", pane as usize))
                            .role(Role::Button)
                            .aria_label(format!("Close {}", pane_label(pane)))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_pane(pane, cx)),
                            ),
                    ),
            )
    }

    fn pane_header_button(
        &self,
        label: &str,
        accessible_label: &str,
        pane: PaneId,
        delta: f32,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let id = if delta < 0. {
            "pane-size-decrease"
        } else if delta > 0. {
            "pane-size-increase"
        } else {
            "pane-size-reset"
        };
        button(label, false)
            .id((id, pane as usize))
            .role(Role::Button)
            .aria_label(format!("{} {}", accessible_label, pane_label(pane)))
            .on_click(cx.listener(move |this, _, _, cx| {
                if delta == 0. {
                    this.reset_pane_size(pane, cx);
                } else {
                    this.resize_pane_by(pane, delta, cx);
                }
            }))
    }

    fn render_resize_handle(
        &self,
        pane: PaneId,
        vertical: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let start_offset = Arc::new(Mutex::new(None));
        let drag = PaneResizeDrag {
            pane,
            start_size: self.pane_size(pane),
            start_offset,
        };
        div()
            .id(("pane-resize-handle", pane as usize))
            .role(Role::Splitter)
            .aria_label(format!(
                "Resize {}. Arrow keys adjust; Return resets.",
                pane_label(pane)
            ))
            .key_context("PaneResizeHandle")
            .tab_index(0)
            .focus_visible(|style| style.bg(Theme::CYAN))
            .when(vertical, |handle| {
                handle.w(px(5.)).h_full().cursor_col_resize()
            })
            .when(!vertical, |handle| {
                handle.h(px(5.)).w_full().cursor_row_resize()
            })
            .bg(Theme::BORDER)
            .hover(|style| style.bg(Theme::CYAN))
            .on_action(cx.listener(move |this, _: &DecreasePaneSize, _, cx| {
                this.resize_pane_by(pane, -24., cx)
            }))
            .on_action(cx.listener(move |this, _: &IncreasePaneSize, _, cx| {
                this.resize_pane_by(pane, 24., cx)
            }))
            .on_action(
                cx.listener(move |this, _: &ResetPaneSize, _, cx| this.reset_pane_size(pane, cx)),
            )
            .on_drag(drag, |drag: &PaneResizeDrag, offset, _, cx| {
                *drag.start_offset.lock().expect("pane resize offset") = Some(offset);
                cx.new(|_| PaneResizePreview)
            })
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<PaneResizeDrag>, _, cx| {
                    let drag = event.drag(cx);
                    let Some(start) = *drag.start_offset.lock().expect("pane resize offset") else {
                        return;
                    };
                    let relative = event.event.position - event.bounds.origin;
                    let delta: f32 = match drag.pane {
                        PaneId::PrimarySidebar => (relative.x - start.x).into(),
                        PaneId::Inspector => (start.x - relative.x).into(),
                        PaneId::BottomPanel => (start.y - relative.y).into(),
                    };
                    this.resize_pane_to(drag.pane, drag.start_size + delta, cx);
                }),
            )
    }

    fn pane_visible(&self, pane: PaneId) -> bool {
        match pane {
            PaneId::PrimarySidebar => self.model.state.panes.primary_sidebar_visible,
            PaneId::Inspector => self.model.state.panes.inspector_visible,
            PaneId::BottomPanel => self.model.state.panes.bottom_panel_visible,
        }
    }

    fn pane_size(&self, pane: PaneId) -> f32 {
        match pane {
            PaneId::PrimarySidebar => self.model.state.panes.primary_sidebar_width,
            PaneId::Inspector => self.model.state.panes.inspector_width,
            PaneId::BottomPanel => self.model.state.panes.bottom_panel_height,
        }
    }

    fn active_editor_uses_embedded_inspector(&self) -> bool {
        matches!(
            active_kind(&self.model),
            EditorKind::FailureInvestigation | EditorKind::FullTrace
        )
    }
}

fn pane_menu_item(
    id: &'static str,
    label: &'static str,
    shortcut: &'static str,
    visible: bool,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .role(Role::MenuItemCheckBox)
        .aria_toggled(if visible {
            gpui::Toggled::True
        } else {
            gpui::Toggled::False
        })
        .aria_label(format!(
            "{label}, {shortcut}, {}",
            if visible { "shown" } else { "hidden" }
        ))
        .tab_index(0)
        .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
        .px_3()
        .py_2()
        .rounded_sm()
        .flex()
        .items_center()
        .justify_between()
        .cursor_pointer()
        .hover(|style| style.bg(Theme::PANEL_ALT))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(if visible { "✓" } else { "  " })
                .child(label),
        )
        .child(div().text_xs().text_color(Theme::MUTED).child(shortcut))
}

fn adjacent_focus_region(
    current: FocusRegion,
    sidebar_visible: bool,
    inspector_visible: bool,
    bottom_visible: bool,
    reverse: bool,
) -> FocusRegion {
    let mut regions = vec![FocusRegion::Editor];
    if sidebar_visible {
        regions.insert(0, FocusRegion::PrimarySidebar);
    }
    if inspector_visible {
        regions.push(FocusRegion::Inspector);
    }
    if bottom_visible {
        regions.push(FocusRegion::BottomPanel);
    }
    let current = regions
        .iter()
        .position(|region| *region == current)
        .unwrap_or_else(|| {
            regions
                .iter()
                .position(|region| *region == FocusRegion::Editor)
                .unwrap()
        });
    let next = if reverse {
        current.checked_sub(1).unwrap_or(regions.len() - 1)
    } else {
        (current + 1) % regions.len()
    };
    regions[next]
}

const fn pane_label(pane: PaneId) -> &'static str {
    match pane {
        PaneId::PrimarySidebar => "primary sidebar",
        PaneId::Inspector => "inspector",
        PaneId::BottomPanel => "bottom panel",
    }
}

#[cfg(test)]
mod focus_order_tests {
    use super::*;

    #[test]
    fn f6_cycles_only_visible_panes_in_spatial_order() {
        assert_eq!(
            adjacent_focus_region(FocusRegion::PrimarySidebar, true, true, true, false),
            FocusRegion::Editor
        );
        assert_eq!(
            adjacent_focus_region(FocusRegion::Editor, true, true, true, false),
            FocusRegion::Inspector
        );
        assert_eq!(
            adjacent_focus_region(FocusRegion::Inspector, true, true, true, false),
            FocusRegion::BottomPanel
        );
        assert_eq!(
            adjacent_focus_region(FocusRegion::BottomPanel, true, true, true, false),
            FocusRegion::PrimarySidebar
        );
    }

    #[test]
    fn hidden_or_stale_focus_returns_to_a_visible_region() {
        assert_eq!(
            adjacent_focus_region(FocusRegion::Inspector, false, false, false, false),
            FocusRegion::Editor
        );
        assert_eq!(
            adjacent_focus_region(FocusRegion::Editor, true, false, false, true),
            FocusRegion::PrimarySidebar
        );
    }
}
