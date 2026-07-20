use super::*;

pub(super) fn active_kind(model: &WorkbenchModel) -> EditorKind {
    model
        .state
        .active_editor
        .as_ref()
        .and_then(|id| model.state.editors.iter().find(|tab| &tab.id == id))
        .map(|tab| tab.resource.kind())
        .unwrap_or(EditorKind::Welcome)
}

pub(super) fn active_title(model: &WorkbenchModel) -> &'static str {
    match active_kind(model) {
        EditorKind::Welcome => "Getting Started",
        EditorKind::FailureInbox => "Failure Inbox",
        EditorKind::FailureInvestigation => "Failure Investigation",
        EditorKind::FullTrace => "Full Trace",
        EditorKind::Runs => "Runs",
        EditorKind::Sources => "Sources",
        EditorKind::EvalReview => "Eval Review",
        EditorKind::Compare => "Compare",
        EditorKind::Settings => "Settings",
    }
}

pub(super) fn active_breadcrumb(model: &WorkbenchModel) -> String {
    let resource = model.state.active_editor.as_ref().and_then(|id| {
        model
            .state
            .editors
            .iter()
            .find(|tab| &tab.id == id)
            .map(|tab| &tab.resource)
    });
    match resource {
        Some(EditorResource::FailureInvestigation {
            project_id,
            group_id,
        }) => format!(
            "{}  /  Failures  /  {}",
            short_id(project_id),
            short_id(group_id)
        ),
        Some(EditorResource::FullTrace {
            project_id,
            logical_trace_id,
            revision,
            ..
        }) => format!(
            "{}  /  Runs  /  {} · rev {}",
            short_id(project_id),
            short_id(logical_trace_id),
            revision
        ),
        Some(EditorResource::EvalReview {
            project_id,
            candidate_id,
        }) => format!(
            "{}  /  Evals  /  {}",
            short_id(project_id),
            short_id(candidate_id)
        ),
        Some(EditorResource::Compare {
            project_id,
            comparison_id,
        }) => format!(
            "{}  /  Compare  /  {}",
            short_id(project_id),
            short_id(comparison_id)
        ),
        _ => active_title(model).into(),
    }
}

pub(super) fn tab_title(resource: &EditorResource) -> String {
    match resource {
        EditorResource::FailureInvestigation { group_id, .. } => {
            format!("Investigation · {}", short_id(group_id))
        }
        EditorResource::FullTrace {
            logical_trace_id, ..
        } => format!("Trace · {}", short_id(logical_trace_id)),
        EditorResource::EvaluatorStudio => "Quality Checks".into(),
        EditorResource::EvalQueue => "Eval Queue".into(),
        EditorResource::CompareSetup => "Compare".into(),
        other => match other.kind() {
            EditorKind::Welcome => "Welcome",
            EditorKind::FailureInbox => "Failure Inbox",
            EditorKind::FailureInvestigation => "Investigation",
            EditorKind::FullTrace => "Full Trace",
            EditorKind::Runs => "Runs",
            EditorKind::Sources => "Sources",
            EditorKind::EvalReview => "Eval Review",
            EditorKind::Compare => "Compare",
            EditorKind::Settings => "Settings",
        }
        .into(),
    }
}

fn short_id(value: &str) -> &str {
    value.get(..value.len().min(12)).unwrap_or(value)
}

pub(super) fn editor_visible_in_scope(
    resource: &EditorResource,
    scope: &crate::workbench::ProjectScope,
) -> bool {
    let owner = match resource {
        EditorResource::FailureInvestigation { project_id, .. }
        | EditorResource::FullTrace { project_id, .. }
        | EditorResource::EvalReview { project_id, .. }
        | EditorResource::Compare { project_id, .. } => Some(project_id.as_str()),
        _ => None,
    };
    match (scope, owner) {
        (crate::workbench::ProjectScope::Project(active), Some(owner)) => active == owner,
        _ => true,
    }
}
