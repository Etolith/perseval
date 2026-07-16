use super::*;

fn perseval_header_icon() -> gpui::Img {
    static ICON: std::sync::OnceLock<std::sync::Arc<gpui::Image>> = std::sync::OnceLock::new();
    let icon = ICON.get_or_init(|| {
        std::sync::Arc::new(gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            include_bytes!("../../../assets/perseval-header-icon.png").to_vec(),
        ))
    });
    gpui::img(icon.clone())
}

impl WorkbenchShell {
    pub(super) fn render_header(
        &self,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let receiver_attention = if self.health.backpressured {
            Some(("Backpressured", Theme::RED))
        } else if self.health.projection_degraded || self.health.last_error.is_some() {
            Some((
                if compact {
                    "Source warning"
                } else {
                    "Trace source needs attention"
                },
                Theme::AMBER,
            ))
        } else {
            None
        };
        div()
            .id("workbench-header")
            .role(Role::Banner)
            .aria_label("Perseval workbench header")
            .h(px(52.))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px_4()
            .border_b_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_3()
                    .child(
                        perseval_header_icon()
                            .size(px(24.))
                            .rounded(px(6.))
                            .flex_none(),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Perseval"),
                    )
                    .when(!compact, |row| {
                        row.child(div().h_5().w(px(1.)).bg(Theme::BORDER)).child(
                            div()
                                .text_xs()
                                .text_color(Theme::MUTED)
                                .child(active_breadcrumb(&self.model)),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .when(!compact, |row| {
                        row.child(scope_chip("Run scope", &self.secondary_scope_label()))
                    })
                    .when(compact && self.has_secondary_scope(), |row| {
                        row.child(
                            status_chip("Scoped", Theme::CYAN)
                                .id("compact-run-scope")
                                .role(Role::Status)
                                .aria_label(format!("Run scope: {}", self.secondary_scope_label())),
                        )
                    })
                    .child(self.render_pane_controls(compact, cx))
                    .child(self.render_project_switcher(cx))
                    .when_some(receiver_attention, |row, receiver| {
                        row.child(
                            status_chip(receiver.0, receiver.1)
                                .id("receiver-status")
                                .role(Role::Status)
                                .aria_label(format!("Trace receiver: {}", receiver.0)),
                        )
                    }),
            )
    }

    fn render_project_switcher(&self, cx: &mut Context<Self>) -> Div {
        let mut menu = div()
            .id("project-scope-menu")
            .role(Role::Menu)
            .aria_label("Project scope")
            .absolute()
            .top(px(38.))
            .right_0()
            .w(px(260.))
            .max_h(px(420.))
            .overflow_y_scroll()
            .p_2()
            .rounded(px(6.))
            .border_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL)
            .shadow_lg()
            .child(
                div()
                    .px_3()
                    .pt_1()
                    .pb_2()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(Theme::MUTED)
                    .child("Switch project"),
            )
            .when(self.projects.is_empty(), |menu| {
                menu.child(
                    div()
                        .px_3()
                        .pb_2()
                        .text_xs()
                        .text_color(Theme::MUTED)
                        .child("Create a project to keep its runs, failures, and evals together."),
                )
            })
            .when(!self.projects.is_empty(), |menu| {
                menu.child(
                    div()
                        .id("project-scope-all")
                        .role(Role::MenuItem)
                        .aria_selected(self.selected_project_id().is_none())
                        .aria_label("All Projects, portfolio view, mutations disabled")
                        .tab_index(0)
                        .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                        .px_3()
                        .py_2()
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(|style| style.bg(Theme::PANEL_ALT))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("All Projects"),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(Theme::AMBER)
                                .child("Portfolio view · mutations disabled"),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_project_scope(crate::workbench::ProjectScope::AllProjects, cx)
                        })),
                )
            });
        for (index, project) in self.projects.iter().enumerate() {
            let project_id = project.project_id.clone();
            let selected = self.selected_project_id() == Some(project.project_id.as_str());
            menu = menu.child(
                div()
                    .id(("project-scope-option", index))
                    .role(Role::MenuItem)
                    .aria_selected(selected)
                    .aria_label(project.display_name.clone())
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .mt_1()
                    .px_3()
                    .py_2()
                    .rounded_sm()
                    .bg(if selected {
                        Theme::ACCENT_MUTED
                    } else {
                        Theme::PANEL
                    })
                    .cursor_pointer()
                    .hover(|style| style.bg(Theme::PANEL_ALT))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(if selected {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .child(project.display_name.clone()),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_xs()
                            .text_color(Theme::MUTED)
                            .child(project.project_id.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_project_scope(
                            crate::workbench::ProjectScope::Project(project_id.clone()),
                            cx,
                        )
                    })),
            );
        }
        menu = menu.child(div().my_2().h(px(1.)).bg(Theme::BORDER)).child(
            div()
                .id("project-scope-create")
                .role(Role::MenuItem)
                .aria_label("Create a new project")
                .tab_index(0)
                .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                .h(px(36.))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .rounded_sm()
                .cursor_pointer()
                .hover(|style| style.bg(Theme::PANEL_ALT))
                .child(icon(AppIcon::Plus, 15., false))
                .child("New project…")
                .on_click(cx.listener(|this, _, _, cx| this.create_project_from_switcher(cx))),
        );
        if self.selected_project_id().is_some() {
            menu = menu.child(
                div()
                    .id("project-scope-manage")
                    .role(Role::MenuItem)
                    .aria_label("Manage project trace sources")
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .h(px(36.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(Theme::PANEL_ALT))
                    .child(icon(AppIcon::Sources, 15., false))
                    .child("Manage trace sources")
                    .on_click(cx.listener(|this, _, _, cx| this.manage_project_sources(cx))),
            );
        }
        div()
            .relative()
            .min_w_0()
            .child(
                scope_chip("Project", &self.project_scope_label())
                    .id("project-scope-switcher")
                    .role(Role::Button)
                    .aria_label(format!("Project scope: {}", self.project_scope_label()))
                    .aria_expanded(self.project_menu_open)
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .cursor_pointer()
                    .bg(if self.project_menu_open {
                        Theme::ACCENT_MUTED
                    } else {
                        Theme::PANEL
                    })
                    .hover(|style| style.bg(Theme::PANEL_ALT))
                    .child(icon(AppIcon::ChevronDown, 13., self.project_menu_open))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_project_menu(cx))),
            )
            .when(self.project_menu_open, |switcher| {
                switcher.child(deferred(menu.occlude()).with_priority(1))
            })
    }

    pub(super) fn render_activity_rail(
        &self,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let mut rail = div()
            .id("activity-rail")
            .role(Role::Navigation)
            .aria_label("Primary navigation")
            .w(px(if compact { 44. } else { 56. }))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .py_2()
            .border_r_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL);
        for (index, (activity, glyph, label, shortcut)) in [
            (ActivityId::Failures, AppIcon::Inbox, "Failure Inbox", "⌘1"),
            (ActivityId::Runs, AppIcon::Runs, "Runs", "⌘2"),
            (ActivityId::Compare, AppIcon::Compare, "Compare", "⌘3"),
            (ActivityId::Evals, AppIcon::Evals, "Evals", "⌘4"),
            (ActivityId::Sources, AppIcon::Sources, "Sources", "⌘5"),
        ]
        .into_iter()
        .enumerate()
        {
            let active = self.model.state.active_activity == activity;
            rail = rail.child(
                activity_button(index, glyph, label, shortcut, active)
                    .on_click(cx.listener(move |this, _, _, cx| this.open_activity(activity, cx))),
            );
        }
        rail.child(div().flex_1()).child(
            activity_button(
                5,
                AppIcon::Settings,
                "Settings",
                "⌘,",
                self.model.state.active_activity == ActivityId::Settings,
            )
            .on_click(cx.listener(|this, _, _, cx| this.open_activity(ActivityId::Settings, cx))),
        )
    }

    pub(super) fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tabs = div()
            .id("editor-tabs")
            .role(Role::TabList)
            .aria_label("Open editors")
            .h(px(40.))
            .flex_none()
            .flex()
            .items_end()
            .overflow_x_scroll()
            .px_3()
            .border_b_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL);
        for (index, tab) in self
            .model
            .state
            .editors
            .iter()
            .enumerate()
            .filter(|(_, tab)| {
                editor_visible_in_scope(&tab.resource, &self.model.state.scope.project)
            })
        {
            let id = tab.id.clone();
            let pin_id = tab.id.clone();
            let close_id = tab.id.clone();
            let active = self.model.state.active_editor.as_ref() == Some(&tab.id);
            let pinned = tab.pinned;
            tabs = tabs.child(
                div()
                    .id(("editor-tab", index))
                    .role(Role::Tab)
                    .aria_label(if pinned {
                        tab_title(&tab.resource)
                    } else {
                        format!("{}; preview", tab_title(&tab.resource))
                    })
                    .aria_selected(active)
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .h(px(36.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_1()
                    .border_b_0()
                    .border_color(Theme::BORDER)
                    .rounded_t_sm()
                    .bg(if active { Theme::BG } else { Theme::PANEL })
                    .text_xs()
                    .text_color(if active { Theme::TEXT } else { Theme::MUTED })
                    .cursor_pointer()
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.activate_editor(id.clone(), cx)),
                    )
                    .child(tab_title(&tab.resource))
                    .when(!pinned, |tab_view| {
                        tab_view.child(
                            div()
                                .id(("pin-editor", index))
                                .role(Role::Button)
                                .aria_label(format!("Pin {}", tab_title(&tab.resource)))
                                .tab_index(0)
                                .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                                .size(px(24.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_sm()
                                .hover(|style| style.bg(Theme::PANEL_ALT))
                                .child(icon(AppIcon::Pin, 14., true))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.pin_editor(pin_id.clone(), cx)
                                })),
                        )
                    })
                    .child(
                        div()
                            .id(("close-editor", index))
                            .role(Role::Button)
                            .aria_label(format!("Close {}", tab_title(&tab.resource)))
                            .tab_index(0)
                            .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                            .px_1()
                            .text_color(Theme::DIM)
                            .child("×")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_editor(close_id.clone(), cx)
                            })),
                    ),
            );
        }
        tabs
    }

    pub(super) fn render_status_bar(&self, compact: bool) -> gpui::Stateful<Div> {
        div()
            .id("workbench-status")
            .role(Role::Status)
            .aria_label(if self.health.enabled {
                format!(
                    "Receiver listening at {}. Queue {} of {}. Journal lag {}. {} runs live. Analysis pending {}.",
                    self.endpoint,
                    self.health.queue_batches,
                    self.health.queue_batch_capacity,
                    self.health.journal_lag,
                    self.health.live_runs + self.health.reopened_runs,
                    self.health.analysis_pending
                )
            } else {
                "Trace receiver disabled".into()
            })
            .h(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .border_t_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL)
            .text_xs()
            .text_color(Theme::MUTED)
            .child(if self.health.enabled {
                format!("● {}", self.endpoint)
            } else {
                "● Receiver disabled".into()
            })
            .when(!compact, |bar| {
                bar.child(format!(
                    "queue {}/{} · journal lag {} · {} live · analysis {} pending",
                    self.health.queue_batches,
                    self.health.queue_batch_capacity,
                    self.health.journal_lag,
                    self.health.live_runs + self.health.reopened_runs,
                    self.health.analysis_pending
                ))
            })
            .when_some(self.persistence_error.clone(), |bar, error| {
                bar.child(format!("Layout not saved · {error}"))
            })
    }
}
