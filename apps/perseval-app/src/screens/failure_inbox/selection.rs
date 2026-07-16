use super::*;

pub(super) fn occurrence_navigation_state(
    offset: u64,
    selected_index: usize,
    loaded_count: usize,
    total_count: u64,
) -> (bool, bool) {
    let previous = selected_index > 0 || offset > 0;
    let next = selected_index + 1 < loaded_count || offset + (loaded_count as u64) < total_count;
    (previous, next)
}

fn adjacent_group_index(current: Option<usize>, count: usize, delta: isize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match current {
        Some(current) => current.saturating_add_signed(delta).min(count - 1),
        None if delta < 0 => count - 1,
        None => 0,
    })
}

fn inclusive_selection_indices(
    anchor: usize,
    target: usize,
    count: usize,
) -> std::ops::RangeInclusive<usize> {
    let last = count.saturating_sub(1);
    anchor.min(target).min(last)..=anchor.max(target).min(last)
}

pub(super) fn reconcile_group_identity_state(
    available: &BTreeSet<(String, String)>,
    focused: &mut Option<(String, String)>,
    selected_group_ids: &mut BTreeSet<String>,
    selection_anchor: &mut Option<(String, String)>,
    previous_focus: Option<(String, String)>,
) {
    *focused = previous_focus.filter(|key| available.contains(key));
    selected_group_ids.retain(|group_id| {
        available
            .iter()
            .any(|(_, available_group_id)| available_group_id == group_id)
    });
    *selection_anchor = selection_anchor
        .take()
        .filter(|key| available.contains(key));
}

impl FailureInbox {
    pub(super) fn open_only_selected_group(&mut self, cx: &mut Context<Self>) {
        if self.selected_group_ids.len() != 1 {
            return;
        }
        let selected = self.selected_group_ids.iter().next().cloned();
        if let Some((project_id, group_id)) = selected.and_then(|group_id| {
            self.groups
                .iter()
                .find(|group| group.group_id == group_id)
                .map(|group| (group.project_id.clone(), group.group_id.clone()))
        }) {
            self.open_group(project_id, group_id, cx);
        }
    }

    pub(super) fn move_primary_focus(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.showing_inbox {
            self.move_group_focus(delta, cx);
            return;
        }

        if self.full_trace {
            let spans = self
                .full_trace_tree
                .visible_rows()
                .into_iter()
                .filter_map(|row| match row {
                    full_trace_tree::FullTraceListRow::Span(span) => Some(*span),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let current = self
                .focused_span_id
                .as_deref()
                .and_then(|span_id| spans.iter().position(|span| span.span_id == span_id));
            if let Some(next) = adjacent_group_index(current, spans.len(), delta)
                && let Some(span) = spans.get(next).cloned()
            {
                self.focus_full_trace_span(span, cx);
            }
            return;
        }

        let Some(evidence) = self.evidence.as_ref() else {
            return;
        };
        let current = self.focused_span_id.as_deref().and_then(|span_id| {
            evidence
                .spans
                .iter()
                .position(|span| span.span_id == span_id)
        });
        if let Some(next) = adjacent_group_index(current, evidence.spans.len(), delta)
            && let Some(span_id) = evidence.spans.get(next).map(|span| span.span_id.clone())
        {
            self.focus_evidence_span(span_id, cx);
        }
    }

    pub(super) fn interact_with_group(
        &mut self,
        project_id: String,
        group_id: String,
        shift: bool,
        secondary: bool,
        toggle_on_plain_click: bool,
        cx: &mut Context<Self>,
    ) {
        self.focused_group = Some((project_id.clone(), group_id.clone()));
        if !self.can_generate_eval() {
            cx.notify();
            return;
        }

        if shift {
            let anchor = self
                .selection_anchor
                .clone()
                .unwrap_or_else(|| (project_id.clone(), group_id.clone()));
            let anchor_index = self
                .groups
                .iter()
                .position(|group| group.project_id == anchor.0 && group.group_id == anchor.1);
            let target_index = self
                .groups
                .iter()
                .position(|group| group.project_id == project_id && group.group_id == group_id);
            if let (Some(anchor_index), Some(target_index)) = (anchor_index, target_index) {
                if !secondary {
                    self.selected_group_ids.clear();
                }
                for index in
                    inclusive_selection_indices(anchor_index, target_index, self.groups.len())
                {
                    self.selected_group_ids
                        .insert(self.groups[index].group_id.clone());
                }
            }
        } else {
            self.selection_anchor = Some((project_id, group_id.clone()));
            if (secondary || toggle_on_plain_click) && !self.selected_group_ids.remove(&group_id) {
                self.selected_group_ids.insert(group_id);
            }
        }
        cx.notify();
    }

    pub(super) fn move_group_focus(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.groups.is_empty() {
            return;
        }
        let current = self
            .focused_group
            .as_ref()
            .and_then(|(project_id, group_id)| {
                self.groups.iter().position(|group| {
                    &group.project_id == project_id && &group.group_id == group_id
                })
            });
        let Some(next) = adjacent_group_index(current, self.groups.len(), delta) else {
            return;
        };
        let group = &self.groups[next];
        self.focused_group = Some((group.project_id.clone(), group.group_id.clone()));
        self.group_scroll
            .scroll_to_item(next, ScrollStrategy::Nearest);
        cx.notify();
    }

    pub(super) fn extend_group_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some((project_id, group_id)) = self.focused_group.clone() else {
            self.move_group_focus(delta, cx);
            return;
        };
        let Some(current) = self
            .groups
            .iter()
            .position(|group| group.project_id == project_id && group.group_id == group_id)
        else {
            return;
        };
        let Some(next) = adjacent_group_index(Some(current), self.groups.len(), delta) else {
            return;
        };
        self.selection_anchor.get_or_insert((project_id, group_id));
        let next_group = &self.groups[next];
        self.interact_with_group(
            next_group.project_id.clone(),
            next_group.group_id.clone(),
            true,
            false,
            false,
            cx,
        );
        self.group_scroll
            .scroll_to_item(next, ScrollStrategy::Nearest);
    }

    pub(super) fn toggle_focused_group(&mut self, cx: &mut Context<Self>) {
        if !self.showing_inbox || !self.can_generate_eval() {
            return;
        }
        let Some((project_id, group_id)) = self.focused_group.clone() else {
            return;
        };
        self.toggle_group(project_id, group_id, cx);
    }

    pub(super) fn toggle_group(
        &mut self,
        project_id: String,
        group_id: String,
        cx: &mut Context<Self>,
    ) {
        if !self.selected_group_ids.remove(&group_id) {
            self.selected_group_ids.insert(group_id.clone());
        }
        self.focused_group = Some((project_id, group_id));
        self.selection_anchor = self.focused_group.clone();
        cx.notify();
    }
}

#[cfg(test)]
mod identity_tests {
    use super::{
        adjacent_group_index, inclusive_selection_indices, occurrence_navigation_state,
        reconcile_group_identity_state,
    };
    use std::collections::BTreeSet;

    #[test]
    fn committed_updates_keep_stable_selection_and_drop_only_missing_groups() {
        let available = BTreeSet::from([
            ("project".to_owned(), "group-b".to_owned()),
            ("project".to_owned(), "group-c".to_owned()),
        ]);
        let mut focused = Some(("project".into(), "group-b".into()));
        let mut selected = BTreeSet::from([
            "group-a".to_owned(),
            "group-b".to_owned(),
            "group-c".to_owned(),
        ]);
        let mut anchor = Some(("project".into(), "group-c".into()));
        reconcile_group_identity_state(
            &available,
            &mut focused,
            &mut selected,
            &mut anchor,
            Some(("project".into(), "group-b".into())),
        );
        assert_eq!(focused, Some(("project".into(), "group-b".into())));
        assert_eq!(
            selected,
            BTreeSet::from(["group-b".into(), "group-c".into()])
        );
        assert_eq!(anchor, Some(("project".into(), "group-c".into())));
    }

    #[test]
    fn resync_never_retargets_focus_to_a_different_group() {
        let available = BTreeSet::from([("project".to_owned(), "group-c".to_owned())]);
        let mut focused = Some(("project".into(), "group-b".into()));
        let mut selected = BTreeSet::from(["group-b".to_owned()]);
        let mut anchor = Some(("project".into(), "group-b".into()));
        reconcile_group_identity_state(
            &available,
            &mut focused,
            &mut selected,
            &mut anchor,
            Some(("project".into(), "group-b".into())),
        );
        assert_eq!(focused, None);
        assert!(selected.is_empty());
        assert_eq!(anchor, None);
    }

    #[test]
    fn occurrence_navigation_disables_boundaries_and_crosses_pages() {
        assert_eq!(occurrence_navigation_state(0, 0, 1, 1), (false, false));
        assert_eq!(occurrence_navigation_state(0, 1, 3, 3), (true, true));
        assert_eq!(occurrence_navigation_state(0, 99, 100, 101), (true, true));
        assert_eq!(occurrence_navigation_state(100, 0, 1, 101), (true, false));
    }

    #[test]
    fn group_keyboard_navigation_is_bounded_and_starts_at_an_edge() {
        assert_eq!(adjacent_group_index(None, 0, 1), None);
        assert_eq!(adjacent_group_index(None, 3, 1), Some(0));
        assert_eq!(adjacent_group_index(None, 3, -1), Some(2));
        assert_eq!(adjacent_group_index(Some(0), 3, -1), Some(0));
        assert_eq!(adjacent_group_index(Some(2), 3, 1), Some(2));
    }

    #[test]
    fn range_selection_is_inclusive_bounded_and_direction_independent() {
        assert_eq!(
            inclusive_selection_indices(1, 3, 5).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            inclusive_selection_indices(3, 1, 5).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            inclusive_selection_indices(9, 2, 4).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }
}
