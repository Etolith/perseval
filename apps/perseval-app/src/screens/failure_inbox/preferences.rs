use std::cmp::Ordering;

use gpui::{Context, Div, FontWeight, Role, div, prelude::*};
use perseval_service::FailureGroupSummary;

use super::components::button;
use super::*;
use crate::workbench::{FailureInboxViewV1, SavedFailureInboxViewV1};

impl FailureInbox {
    pub(crate) fn set_preferences(
        &mut self,
        preferences: FailureInboxPreferencesV1,
        cx: &mut Context<Self>,
    ) {
        if self.preferences == preferences {
            return;
        }
        self.preferences = preferences;
        self.apply_current_view(cx);
    }

    pub(super) fn sort_groups(&mut self) {
        let sort = self.preferences.current.sort;
        if sort == FailureInboxSort::Priority {
            return;
        }
        self.groups.sort_by(|left, right| {
            let ordering = match sort {
                FailureInboxSort::Priority => Ordering::Equal,
                FailureInboxSort::MostRecent => right.last_seen_at.cmp(&left.last_seen_at),
                FailureInboxSort::MostFrequent => {
                    right.occurrence_count.cmp(&left.occurrence_count)
                }
                FailureInboxSort::MostUnresolved => unresolved(right).cmp(&unresolved(left)),
            };
            ordering
                .then_with(|| right.last_seen_at.cmp(&left.last_seen_at))
                .then_with(|| left.project_id.cmp(&right.project_id))
                .then_with(|| left.group_id.cmp(&right.group_id))
        });
    }

    pub(super) fn current_sort(&self) -> FailureInboxSort {
        self.preferences.current.sort
    }

    pub(super) fn select_sort(&mut self, sort: FailureInboxSort, cx: &mut Context<Self>) {
        self.preferences.current.sort = sort;
        self.open_filter_menu = None;
        self.reload_groups(cx);
        self.commit_current_view(cx);
    }

    pub(super) fn save_current_view(&mut self, cx: &mut Context<Self>) {
        self.capture_current_view();
        if let Some(existing) = self
            .preferences
            .saved
            .iter()
            .find(|saved| saved.view == self.preferences.current)
        {
            self.preferences.active_saved_view_id = Some(existing.id.clone());
        } else {
            let id = next_saved_view_id(&self.preferences);
            let saved = SavedFailureInboxViewV1 {
                id: id.clone(),
                name: describe_view(&self.preferences.current),
                view: self.preferences.current.clone(),
            };
            self.preferences.saved.push(saved);
            self.preferences.active_saved_view_id = Some(id);
        }
        self.open_filter_menu = None;
        self.emit_preferences(cx);
        cx.notify();
    }

    pub(super) fn activate_saved_view(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(view) = self
            .preferences
            .saved
            .iter()
            .find(|saved| saved.id == id)
            .map(|saved| saved.view.clone())
        else {
            return;
        };
        self.preferences.current = view;
        self.preferences.active_saved_view_id = Some(id);
        self.open_filter_menu = None;
        self.apply_current_view(cx);
        self.emit_preferences(cx);
    }

    pub(super) fn remove_saved_view(&mut self, id: String, cx: &mut Context<Self>) {
        self.preferences.saved.retain(|saved| saved.id != id);
        if self.preferences.active_saved_view_id.as_deref() == Some(id.as_str()) {
            self.preferences.active_saved_view_id = None;
        }
        self.emit_preferences(cx);
        cx.notify();
    }

    pub(super) fn commit_current_view(&mut self, cx: &mut Context<Self>) {
        self.capture_current_view();
        self.preferences.active_saved_view_id = self
            .preferences
            .saved
            .iter()
            .find(|saved| saved.view == self.preferences.current)
            .map(|saved| saved.id.clone());
        self.emit_preferences(cx);
    }

    pub(super) fn render_preferences_menu(&self, cx: &mut Context<Self>) -> Option<Div> {
        let menu = self.open_filter_menu?;
        if menu != InboxFilterMenu::Organize {
            return None;
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

        let mut sort_row = preference_section("Sort");
        for (index, sort) in [
            FailureInboxSort::Priority,
            FailureInboxSort::MostRecent,
            FailureInboxSort::MostFrequent,
            FailureInboxSort::MostUnresolved,
        ]
        .into_iter()
        .enumerate()
        {
            sort_row = sort_row.child(
                button(sort.label(), self.current_sort() == sort)
                    .id(("failure-sort-option", index))
                    .role(Role::MenuItemRadio)
                    .aria_selected(self.current_sort() == sort)
                    .on_click(cx.listener(move |this, _, _, cx| this.select_sort(sort, cx))),
            );
        }
        panel = panel.child(sort_row);

        let mut views = preference_section("Views").child(
            button("Save current view", false)
                .id("save-current-failure-view")
                .role(Role::Button)
                .aria_label("Save current failure filters and sort")
                .on_click(cx.listener(|this, _, _, cx| this.save_current_view(cx))),
        );
        if self.preferences.saved.is_empty() {
            views = views.child(
                div()
                    .text_xs()
                    .text_color(Theme::DIM)
                    .child("No saved views yet"),
            );
        }
        for (index, saved) in self.preferences.saved.iter().cloned().enumerate() {
            let active = self.preferences.active_saved_view_id.as_deref() == Some(&saved.id);
            let activate_id = saved.id.clone();
            let remove_id = saved.id.clone();
            views = views
                .child(
                    button(&saved.name, active)
                        .id(("saved-failure-view", index))
                        .role(Role::MenuItemRadio)
                        .aria_selected(active)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.activate_saved_view(activate_id.clone(), cx)
                        })),
                )
                .child(
                    button("Remove", false)
                        .id(("remove-saved-failure-view", index))
                        .role(Role::Button)
                        .aria_label(format!("Remove saved view {}", saved.name))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_saved_view(remove_id.clone(), cx)
                        })),
                );
        }
        Some(panel.child(views))
    }

    fn apply_current_view(&mut self, cx: &mut Context<Self>) {
        let view = self.preferences.current.clone();
        self.filters.severity = view.severity;
        self.filters.recovery = view.recovery;
        self.filters.detector_id = view.detector_id.clone();
        let mut criteria = self.filters.scope.criteria.clone();
        criteria.service_name = view.service_name.clone();
        self.filters.scope = QueryScopeV1::new(criteria);
        self.filters.search = view.search.clone();
        self.search_input.update(cx, |input, cx| {
            input.set_text(view.search.clone().unwrap_or_default(), cx)
        });
        self.reload_groups(cx);
        cx.notify();
    }

    fn capture_current_view(&mut self) {
        self.preferences.current = FailureInboxViewV1 {
            severity: self.filters.severity,
            recovery: self.filters.recovery,
            detector_id: self.filters.detector_id.clone(),
            service_name: self.filters.scope.criteria.service_name.clone(),
            search: self.filters.search.clone(),
            sort: self.preferences.current.sort,
        };
    }

    fn emit_preferences(&self, cx: &mut Context<Self>) {
        cx.emit(FailureInboxEvent::PreferencesChanged {
            scope_key: self.query_scope.preference_key(),
            preferences: self.preferences.clone(),
        });
    }
}

fn preference_section(label: &str) -> Div {
    div().flex().flex_wrap().items_center().gap_2().child(
        div()
            .w(px(72.))
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .text_color(Theme::DIM)
            .child(label.to_uppercase()),
    )
}

fn unresolved(group: &FailureGroupSummary) -> u64 {
    group.unrecovered_count + group.unknown_recovery_count
}

fn next_saved_view_id(preferences: &FailureInboxPreferencesV1) -> String {
    let mut sequence = 1;
    loop {
        let id = format!("failure-view-{sequence}");
        if preferences.saved.iter().all(|saved| saved.id != id) {
            return id;
        }
        sequence += 1;
    }
}

fn describe_view(view: &FailureInboxViewV1) -> String {
    let mut parts = Vec::new();
    if let Some(severity) = view.severity {
        parts.push(format!("{severity:?}"));
    }
    if let Some(recovery) = view.recovery {
        parts.push(format!("{recovery:?}"));
    }
    if let Some(detector) = view.detector_id.as_deref() {
        parts.push(super::components::humanize(detector));
    }
    if let Some(service) = view.service_name.as_deref() {
        parts.push(service.to_owned());
    }
    if parts.is_empty() {
        parts.push("All failures".into());
    }
    if view.sort != FailureInboxSort::Priority {
        parts.push(view.sort.label().into());
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(id: &str, occurrences: u64, unresolved: u64, last_seen: &str) -> FailureGroupSummary {
        FailureGroupSummary {
            scope: QueryScopeV1::new(QueryScopeCriteriaV1 {
                project_id: Some("project".into()),
                ..QueryScopeCriteriaV1::default()
            }),
            project_id: "project".into(),
            group_id: id.into(),
            failure_signature: id.into(),
            detector_ids: vec![id.into()],
            subject: None,
            operation: None,
            presentation: None,
            severity: FindingSeverity::Medium,
            occurrence_count: occurrences,
            recovered_count: occurrences.saturating_sub(unresolved),
            unrecovered_count: unresolved,
            unknown_recovery_count: 0,
            affected_run_count: occurrences,
            affected_build_count: 0,
            affected_environment_count: 0,
            confirmed_count: 0,
            dismissed_count: 0,
            needs_context_count: 0,
            unreviewed_count: occurrences,
            stale_disposition_count: 0,
            first_seen_at: last_seen.into(),
            last_seen_at: last_seen.into(),
            occurrence_trend: vec![occurrences],
            recurrence: None,
            telemetry_gap_count: 0,
            reanalyzing: false,
            feature_similarity_cohorts: Vec::new(),
        }
    }

    #[test]
    fn saved_view_names_and_ids_are_stable_and_human_readable() {
        let view = FailureInboxViewV1 {
            severity: Some(FindingSeverity::Critical),
            recovery: Some(RecoveryStatus::Unrecovered),
            detector_id: Some("tool_call_loop".into()),
            sort: FailureInboxSort::MostFrequent,
            ..FailureInboxViewV1::default()
        };
        assert_eq!(
            describe_view(&view),
            "Critical · Unrecovered · Tool Call Loop · Most frequent"
        );
        let mut preferences = FailureInboxPreferencesV1::default();
        assert_eq!(next_saved_view_id(&preferences), "failure-view-1");
        preferences.saved.push(SavedFailureInboxViewV1 {
            id: "failure-view-2".into(),
            name: "Existing".into(),
            view,
        });
        assert_eq!(next_saved_view_id(&preferences), "failure-view-1");
    }

    #[test]
    fn supported_sorts_are_deterministic() {
        let groups = vec![
            group("a", 2, 2, "2026-01-01T00:00:00Z"),
            group("b", 8, 1, "2026-01-03T00:00:00Z"),
            group("c", 3, 3, "2026-01-02T00:00:00Z"),
        ];
        let sort = |kind| {
            let mut groups = groups.clone();
            groups.sort_by(|left, right| {
                match kind {
                    FailureInboxSort::MostRecent => right.last_seen_at.cmp(&left.last_seen_at),
                    FailureInboxSort::MostFrequent => {
                        right.occurrence_count.cmp(&left.occurrence_count)
                    }
                    FailureInboxSort::MostUnresolved => unresolved(right).cmp(&unresolved(left)),
                    FailureInboxSort::Priority => Ordering::Equal,
                }
                .then_with(|| left.group_id.cmp(&right.group_id))
            });
            groups
                .into_iter()
                .map(|group| group.group_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(sort(FailureInboxSort::MostRecent), vec!["b", "c", "a"]);
        assert_eq!(sort(FailureInboxSort::MostFrequent), vec!["b", "c", "a"]);
        assert_eq!(sort(FailureInboxSort::MostUnresolved), vec!["c", "a", "b"]);
    }
}
