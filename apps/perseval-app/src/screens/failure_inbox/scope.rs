use std::time::{SystemTime, UNIX_EPOCH};

use gpui::Context;

use super::*;
use crate::workbench::ProjectScope;

impl FailureInbox {
    pub(crate) fn set_query_scope(&mut self, scope: &QueryScope, cx: &mut Context<Self>) {
        if &self.query_scope == scope {
            return;
        }
        let project_id = match &scope.project {
            ProjectScope::Project(project_id) => Some(project_id.clone()),
            ProjectScope::AllProjects => None,
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        let started_after_unix_nano = scope.started_after_unix_nano(now);
        self.query_scope = scope.clone();
        self.filters.scope = QueryScopeV1::new(QueryScopeCriteriaV1 {
            project_id,
            environment: scope.environment.clone(),
            build_id: scope.build.clone(),
            session_id: scope.session.clone(),
            service_name: self.filters.scope.criteria.service_name.clone(),
            started_after_unix_nano,
            started_before_unix_nano: None,
        });
        self.focused_group = None;
        self.selected_group_ids.clear();
        self.selection_anchor = None;
        self.selected_group = None;
        self.occurrences.clear();
        self.evidence = None;
        self.focused_span_id = None;
        self.focused_span_snapshot = None;
        self.batch_preview = None;
        self.generation_job = None;
        self.reload_groups(cx);
        cx.notify();
    }
}
