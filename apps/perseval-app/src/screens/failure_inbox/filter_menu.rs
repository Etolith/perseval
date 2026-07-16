use super::components::*;
use super::*;

impl FailureInbox {
    pub(super) fn toggle_filter_menu(&mut self, menu: InboxFilterMenu, cx: &mut Context<Self>) {
        self.open_filter_menu = (self.open_filter_menu != Some(menu)).then_some(menu);
        cx.notify();
    }

    pub(super) fn select_severity(
        &mut self,
        value: Option<FindingSeverity>,
        cx: &mut Context<Self>,
    ) {
        self.filters.severity = value;
        self.open_filter_menu = Some(InboxFilterMenu::Filters);
        self.reload_groups(cx);
        self.commit_current_view(cx);
        cx.notify();
    }

    pub(super) fn select_recovery(
        &mut self,
        value: Option<RecoveryStatus>,
        cx: &mut Context<Self>,
    ) {
        self.filters.recovery = value;
        self.open_filter_menu = Some(InboxFilterMenu::Filters);
        self.reload_groups(cx);
        self.commit_current_view(cx);
        cx.notify();
    }

    pub(super) fn toggle_fully_dismissed(&mut self, cx: &mut Context<Self>) {
        self.filters.include_fully_dismissed = !self.filters.include_fully_dismissed;
        self.open_filter_menu = Some(InboxFilterMenu::Filters);
        self.reload_groups(cx);
        self.commit_current_view(cx);
        cx.notify();
    }

    pub(super) fn select_detector(&mut self, value: Option<String>, cx: &mut Context<Self>) {
        self.filters.detector_id = value;
        self.open_filter_menu = Some(InboxFilterMenu::Filters);
        self.reload_groups(cx);
        self.commit_current_view(cx);
        cx.notify();
    }

    pub(super) fn select_service(&mut self, value: Option<String>, cx: &mut Context<Self>) {
        let mut criteria = self.filters.scope.criteria.clone();
        criteria.service_name = value;
        self.filters.scope = QueryScopeV1::new(criteria);
        self.open_filter_menu = Some(InboxFilterMenu::Filters);
        self.reload_groups(cx);
        self.commit_current_view(cx);
        cx.notify();
    }

    pub(super) fn apply_search(&mut self, cx: &mut Context<Self>) {
        let value = self.search_input.read(cx).text().trim().to_string();
        let search = (!value.is_empty()).then_some(value);
        if self.filters.search == search {
            return;
        }
        self.filters.search = search;
        self.reload_groups(cx);
        self.commit_current_view(cx);
        cx.notify();
    }

    pub(super) fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.search_request_generation = self.search_request_generation.wrapping_add(1);
        let generation = self.search_request_generation;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |weak, cx| {
            executor.timer(Duration::from_millis(220)).await;
            let _ = weak.update(cx, |this, cx| {
                if this.search_request_generation == generation {
                    this.apply_search(cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn clear_filters(&mut self, cx: &mut Context<Self>) {
        self.filters.severity = None;
        self.filters.recovery = None;
        self.filters.detector_id = None;
        self.filters.include_fully_dismissed = false;
        let mut criteria = self.filters.scope.criteria.clone();
        criteria.service_name = None;
        self.filters.scope = QueryScopeV1::new(criteria);
        self.filters.search = None;
        self.search_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.open_filter_menu = None;
        self.reload_groups(cx);
        self.commit_current_view(cx);
        cx.notify();
    }

    pub(super) fn has_active_group_filters(&self) -> bool {
        self.filters.severity.is_some()
            || self.filters.recovery.is_some()
            || self.filters.detector_id.is_some()
            || self.filters.include_fully_dismissed
            || self.filters.scope.criteria.service_name.is_some()
            || self.filters.search.is_some()
    }

    pub(super) fn active_group_filter_count(&self) -> usize {
        usize::from(self.filters.severity.is_some())
            + usize::from(self.filters.recovery.is_some())
            + usize::from(self.filters.detector_id.is_some())
            + usize::from(self.filters.include_fully_dismissed)
            + usize::from(self.filters.scope.criteria.service_name.is_some())
            + usize::from(self.filters.search.is_some())
    }

    pub(super) fn render_open_filter_menu(&self, cx: &mut Context<Self>) -> Option<Div> {
        let menu = self.open_filter_menu?;
        if menu == InboxFilterMenu::Organize {
            return self.render_preferences_menu(cx);
        }
        let mut panel = div()
            .mt_2()
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_sm()
            .border_1()
            .border_color(Theme::BORDER)
            .bg(Theme::PANEL_ALT);

        let mut severity = filter_section("Severity");
        for (index, value) in [
            None,
            Some(FindingSeverity::Critical),
            Some(FindingSeverity::High),
            Some(FindingSeverity::Medium),
            Some(FindingSeverity::Low),
            Some(FindingSeverity::Info),
        ]
        .into_iter()
        .enumerate()
        {
            severity = severity.child(
                button(
                    &value
                        .map(|value| format!("{value:?}"))
                        .unwrap_or_else(|| "All".into()),
                    self.filters.severity == value,
                )
                .id(("severity-option", index))
                .role(Role::MenuItemRadio)
                .aria_selected(self.filters.severity == value)
                .on_click(cx.listener(move |this, _, _, cx| this.select_severity(value, cx))),
            );
        }
        panel = panel.child(severity);

        let mut recovery = filter_section("Recovery");
        for (index, value) in [
            None,
            Some(RecoveryStatus::Unrecovered),
            Some(RecoveryStatus::Unknown),
            Some(RecoveryStatus::Recovered),
        ]
        .into_iter()
        .enumerate()
        {
            recovery = recovery.child(
                button(
                    &value
                        .map(|value| format!("{value:?}"))
                        .unwrap_or_else(|| "All".into()),
                    self.filters.recovery == value,
                )
                .id(("recovery-option", index))
                .role(Role::MenuItemRadio)
                .aria_selected(self.filters.recovery == value)
                .on_click(cx.listener(move |this, _, _, cx| this.select_recovery(value, cx))),
            );
        }
        recovery = recovery.child(
            button("Include dismissed", self.filters.include_fully_dismissed)
                .id("include-dismissed-groups")
                .role(Role::MenuItemCheckBox)
                .aria_label("Include groups whose current findings are all dismissed")
                .aria_toggled(if self.filters.include_fully_dismissed {
                    Toggled::True
                } else {
                    Toggled::False
                })
                .on_click(cx.listener(|this, _, _, cx| this.toggle_fully_dismissed(cx))),
        );
        panel = panel.child(recovery);

        let mut detectors = filter_section("Detector").child(
            button("All", self.filters.detector_id.is_none())
                .id("detector-option-all")
                .role(Role::MenuItemRadio)
                .aria_selected(self.filters.detector_id.is_none())
                .on_click(cx.listener(|this, _, _, cx| this.select_detector(None, cx))),
        );
        for (index, value) in self.detector_options.iter().cloned().enumerate() {
            let active = self.filters.detector_id.as_deref() == Some(value.as_str());
            let label = humanize(&value);
            detectors = detectors.child(
                button(&label, active)
                    .id(("detector-option", index))
                    .role(Role::MenuItemRadio)
                    .aria_label(label)
                    .aria_selected(active)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_detector(Some(value.clone()), cx)
                    })),
            );
        }
        panel = panel.child(detectors);

        let mut services = filter_section("Service").child(
            button("All", self.filters.scope.criteria.service_name.is_none())
                .id("service-option-all")
                .role(Role::MenuItemRadio)
                .aria_selected(self.filters.scope.criteria.service_name.is_none())
                .on_click(cx.listener(|this, _, _, cx| this.select_service(None, cx))),
        );
        for (index, value) in self.service_options.iter().cloned().enumerate() {
            let active =
                self.filters.scope.criteria.service_name.as_deref() == Some(value.as_str());
            services = services.child(
                button(&value, active)
                    .id(("service-option", index))
                    .role(Role::MenuItemRadio)
                    .aria_label(value.clone())
                    .aria_selected(active)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_service(Some(value.clone()), cx)
                    })),
            );
        }
        panel = panel.child(services).child(
            div().flex().justify_end().child(
                button("Reset filters", false)
                    .id("reset-filters")
                    .role(Role::Button)
                    .aria_label("Reset failure filters")
                    .on_click(cx.listener(|this, _, _, cx| this.clear_filters(cx))),
            ),
        );
        Some(panel)
    }
}

fn filter_section(label: &str) -> Div {
    div().flex().flex_wrap().items_center().gap_2().child(
        div()
            .w(px(72.))
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .text_color(Theme::DIM)
            .child(label.to_uppercase()),
    )
}
