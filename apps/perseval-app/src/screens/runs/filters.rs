use super::*;

impl RunsScreen {
    pub(super) fn render_open_filter_menu(&self, cx: &mut Context<Self>) -> Option<Div> {
        let menu = self.open_filter_menu?;
        let mut panel = div()
            .mt_2()
            .p_2()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .rounded_sm()
            .border_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL_ALT)
            .child(
                div()
                    .mr_1()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(Theme::DIM)
                    .child(match menu {
                        RunsFilterMenu::Environment => "ENVIRONMENT",
                        RunsFilterMenu::Build => "BUILD",
                        RunsFilterMenu::Session => "SESSION",
                        RunsFilterMenu::Lifecycle => "LIFECYCLE",
                        RunsFilterMenu::Identity => "IDENTITY",
                        RunsFilterMenu::Started => "STARTED",
                    }),
            );

        match menu {
            RunsFilterMenu::Environment | RunsFilterMenu::Build | RunsFilterMenu::Session => {
                let (selected, options) = match menu {
                    RunsFilterMenu::Environment => (
                        self.filters.scope.criteria.environment.as_deref(),
                        self.environment_options.clone(),
                    ),
                    RunsFilterMenu::Build => (
                        self.filters.scope.criteria.build_id.as_deref(),
                        self.build_options.clone(),
                    ),
                    RunsFilterMenu::Session => (
                        self.filters.scope.criteria.session_id.as_deref(),
                        self.session_options.clone(),
                    ),
                    _ => unreachable!(),
                };
                panel = panel.child(
                    button("All", selected.is_none())
                        .id(("runs-filter-option-all", menu as usize))
                        .role(Role::MenuItemRadio)
                        .aria_label("All values")
                        .aria_selected(selected.is_none())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_text_filter(menu, None, cx)
                        })),
                );
                let no_options = options.is_empty();
                for (index, value) in options.into_iter().enumerate() {
                    let active = selected == Some(value.as_str());
                    let selected_value = value.clone();
                    panel = panel.child(
                        button(&value, active)
                            .id(format!("runs-filter-option-{}-{index}", menu as usize))
                            .role(Role::MenuItemRadio)
                            .aria_label(value.clone())
                            .aria_selected(active)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_text_filter(menu, Some(selected_value.clone()), cx)
                            })),
                    );
                }
                if no_options {
                    panel = panel.child(
                        div()
                            .text_xs()
                            .text_color(Theme::DIM)
                            .child("No values in loaded runs"),
                    );
                }
            }
            RunsFilterMenu::Lifecycle => {
                for (index, value) in [
                    None,
                    Some(TraceLifecycle::Live),
                    Some(TraceLifecycle::Quiescent),
                    Some(TraceLifecycle::Finalized),
                    Some(TraceLifecycle::Reopened),
                ]
                .into_iter()
                .enumerate()
                {
                    panel = panel.child(
                        button(
                            value.map(lifecycle_label).unwrap_or("All"),
                            self.filters.lifecycle == value,
                        )
                        .id(("runs-lifecycle-option", index))
                        .role(Role::MenuItemRadio)
                        .aria_label(value.map(lifecycle_label).unwrap_or("All"))
                        .aria_selected(self.filters.lifecycle == value)
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.select_lifecycle(value, cx)),
                        ),
                    );
                }
            }
            RunsFilterMenu::Identity => {
                for (index, value) in [
                    None,
                    Some(IdentityQualityV1::Explicit),
                    Some(IdentityQualityV1::Inferred),
                    Some(IdentityQualityV1::Unknown),
                ]
                .into_iter()
                .enumerate()
                {
                    panel = panel.child(
                        button(
                            value.map(identity_filter_label).unwrap_or("All"),
                            self.filters.identity_quality == value,
                        )
                        .id(("runs-identity-option", index))
                        .role(Role::MenuItemRadio)
                        .aria_label(value.map(identity_filter_label).unwrap_or("All"))
                        .aria_selected(self.filters.identity_quality == value)
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.select_identity(value, cx)),
                        ),
                    );
                }
            }
            RunsFilterMenu::Started => {
                for (index, value) in [
                    RunTimeWindow::All,
                    RunTimeWindow::LastHour,
                    RunTimeWindow::LastDay,
                    RunTimeWindow::LastWeek,
                ]
                .into_iter()
                .enumerate()
                {
                    panel = panel.child(
                        button(value.label(), self.time_window == value)
                            .id(("runs-time-option", index))
                            .role(Role::MenuItemRadio)
                            .aria_label(value.label())
                            .aria_selected(self.time_window == value)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_time_window(value, cx)
                            })),
                    );
                }
            }
        }
        Some(panel)
    }
}
