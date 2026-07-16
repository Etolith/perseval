use std::collections::{BTreeMap, BTreeSet};

use perseval_service::analysis::{FindingSeverity, RecoveryStatus};
use serde::{Deserialize, Serialize};

pub const WORKBENCH_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityId {
    Failures,
    Runs,
    Compare,
    Evals,
    Sources,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorKind {
    Welcome,
    FailureInbox,
    FailureInvestigation,
    FullTrace,
    Runs,
    Sources,
    EvalReview,
    Compare,
    Settings,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FullTraceOrigin {
    #[default]
    Runs,
    FailureInvestigation {
        project_id: String,
        group_id: String,
        finding_id: Option<String>,
        occurrence_offset: u64,
        span_id: Option<String>,
    },
}

impl EditorKind {
    pub const fn activity(self) -> ActivityId {
        match self {
            Self::Welcome | Self::Sources => ActivityId::Sources,
            Self::FailureInbox | Self::FailureInvestigation => ActivityId::Failures,
            Self::FullTrace | Self::Runs => ActivityId::Runs,
            Self::EvalReview => ActivityId::Evals,
            Self::Compare => ActivityId::Compare,
            Self::Settings => ActivityId::Settings,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EditorResource {
    Welcome,
    FailureInbox,
    FailureInvestigation {
        project_id: String,
        group_id: String,
    },
    FullTrace {
        project_id: String,
        logical_trace_id: String,
        revision: u64,
        #[serde(default)]
        origin: FullTraceOrigin,
        #[serde(default)]
        selected_span_id: Option<String>,
    },
    Runs,
    Sources,
    EvalQueue,
    EvalReview {
        project_id: String,
        candidate_id: String,
    },
    CompareSetup,
    Compare {
        project_id: String,
        comparison_id: String,
    },
    Settings,
}

impl EditorResource {
    pub const fn is_activity_destination(&self) -> bool {
        matches!(
            self,
            Self::Welcome
                | Self::FailureInbox
                | Self::Runs
                | Self::Sources
                | Self::EvalQueue
                | Self::CompareSetup
                | Self::Settings
        )
    }

    pub const fn kind(&self) -> EditorKind {
        match self {
            Self::Welcome => EditorKind::Welcome,
            Self::FailureInbox => EditorKind::FailureInbox,
            Self::FailureInvestigation { .. } => EditorKind::FailureInvestigation,
            Self::FullTrace { .. } => EditorKind::FullTrace,
            Self::Runs => EditorKind::Runs,
            Self::Sources => EditorKind::Sources,
            Self::EvalQueue => EditorKind::EvalReview,
            Self::EvalReview { .. } => EditorKind::EvalReview,
            Self::CompareSetup => EditorKind::Compare,
            Self::Compare { .. } => EditorKind::Compare,
            Self::Settings => EditorKind::Settings,
        }
    }

    pub fn stable_key(&self) -> String {
        match self {
            Self::Welcome => "welcome".into(),
            Self::FailureInbox => "failure-inbox".into(),
            Self::FailureInvestigation {
                project_id,
                group_id,
            } => format!("failure-investigation:{project_id}:{group_id}"),
            Self::FullTrace {
                project_id,
                logical_trace_id,
                revision,
                ..
            } => format!("full-trace:{project_id}:{logical_trace_id}:{revision}"),
            Self::Runs => "runs".into(),
            Self::Sources => "sources".into(),
            Self::EvalQueue => "eval-queue".into(),
            Self::EvalReview {
                project_id,
                candidate_id,
            } => format!("eval-review:{project_id}:{candidate_id}"),
            Self::CompareSetup => "compare-setup".into(),
            Self::Compare {
                project_id,
                comparison_id,
            } => format!("compare:{project_id}:{comparison_id}"),
            Self::Settings => "settings".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EditorId(pub String);

impl EditorId {
    pub fn for_resource(resource: &EditorResource) -> Self {
        Self(resource.stable_key())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorTabState {
    pub id: EditorId,
    pub resource: EditorResource,
    pub pinned: bool,
}

impl EditorTabState {
    pub fn new(resource: EditorResource, pinned: bool) -> Self {
        Self {
            id: EditorId::for_resource(&resource),
            resource,
            pinned,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneLayout {
    pub primary_sidebar_visible: bool,
    pub inspector_visible: bool,
    pub bottom_panel_visible: bool,
    pub primary_sidebar_width: f32,
    pub inspector_width: f32,
    pub bottom_panel_height: f32,
}

impl Default for PaneLayout {
    fn default() -> Self {
        Self {
            primary_sidebar_visible: false,
            inspector_visible: false,
            bottom_panel_visible: false,
            primary_sidebar_width: 280.,
            inspector_width: 360.,
            bottom_panel_height: 220.,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectScope {
    AllProjects,
    Project(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryScope {
    pub project: ProjectScope,
    pub environment: Option<String>,
    pub build: Option<String>,
    pub session: Option<String>,
    pub time_range: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectContextState {
    pub environment: Option<String>,
    pub build: Option<String>,
    pub session: Option<String>,
    pub time_range: Option<String>,
    pub active_activity: ActivityId,
    pub active_editor: Option<EditorId>,
    pub panes: PaneLayout,
}

impl Default for ProjectContextState {
    fn default() -> Self {
        Self {
            environment: None,
            build: None,
            session: None,
            time_range: None,
            active_activity: ActivityId::Failures,
            active_editor: Some(EditorId("failure-inbox".into())),
            panes: PaneLayout::default(),
        }
    }
}

impl Default for QueryScope {
    fn default() -> Self {
        Self {
            project: ProjectScope::AllProjects,
            environment: None,
            build: None,
            session: None,
            time_range: None,
        }
    }
}

impl QueryScope {
    pub fn preference_key(&self) -> String {
        match &self.project {
            ProjectScope::AllProjects => "all-projects".into(),
            ProjectScope::Project(project_id) => format!("project:{project_id}"),
        }
    }

    pub fn allows_mutation(&self) -> bool {
        matches!(self.project, ProjectScope::Project(_))
    }

    pub fn started_after_unix_nano(&self, now: u64) -> Option<u64> {
        let seconds = match self.time_range.as_deref()? {
            "last_hour" => 60 * 60,
            "last_day" => 24 * 60 * 60,
            "last_week" => 7 * 24 * 60 * 60,
            _ => return None,
        };
        Some(now.saturating_sub(seconds * 1_000_000_000))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OnboardingState {
    pub completed: bool,
    pub dismissed: bool,
    pub current_step: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BulkSelection {
    pub failure_group_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FailureInboxSort {
    #[default]
    Priority,
    MostRecent,
    MostFrequent,
    MostUnresolved,
}

impl FailureInboxSort {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Priority => "Priority",
            Self::MostRecent => "Most recent",
            Self::MostFrequent => "Most frequent",
            Self::MostUnresolved => "Most unresolved",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FailureInboxViewV1 {
    pub severity: Option<FindingSeverity>,
    pub recovery: Option<RecoveryStatus>,
    pub detector_id: Option<String>,
    pub service_name: Option<String>,
    pub search: Option<String>,
    pub sort: FailureInboxSort,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedFailureInboxViewV1 {
    pub id: String,
    pub name: String,
    pub view: FailureInboxViewV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FailureInboxPreferencesV1 {
    pub current: FailureInboxViewV1,
    pub saved: Vec<SavedFailureInboxViewV1>,
    pub active_saved_view_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TextScale {
    #[default]
    Normal,
    Large,
    ExtraLarge,
    Double,
}

impl TextScale {
    pub const fn percent(self) -> u16 {
        match self {
            Self::Normal => 100,
            Self::Large => 125,
            Self::ExtraLarge => 150,
            Self::Double => 200,
        }
    }

    pub const fn factor(self) -> f32 {
        self.percent() as f32 / 100.
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppearancePreferencesV1 {
    pub text_scale: TextScale,
    pub reduced_motion: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkbenchStateV1 {
    pub schema_version: u32,
    pub active_activity: ActivityId,
    pub editors: Vec<EditorTabState>,
    pub active_editor: Option<EditorId>,
    pub panes: PaneLayout,
    pub scope: QueryScope,
    pub onboarding: OnboardingState,
    pub bulk_selection: BulkSelection,
    #[serde(default)]
    pub project_contexts: BTreeMap<String, ProjectContextState>,
    #[serde(default)]
    pub failure_inbox_preferences: BTreeMap<String, FailureInboxPreferencesV1>,
    #[serde(default)]
    pub inspector_auto_open_suppressed: bool,
    #[serde(default)]
    pub appearance: AppearancePreferencesV1,
    pub focus: super::FocusRegion,
}

impl Default for WorkbenchStateV1 {
    fn default() -> Self {
        let inbox = EditorTabState::new(EditorResource::FailureInbox, false);
        Self {
            schema_version: WORKBENCH_STATE_VERSION,
            active_activity: ActivityId::Failures,
            active_editor: Some(inbox.id.clone()),
            editors: vec![inbox],
            panes: PaneLayout::default(),
            scope: QueryScope::default(),
            onboarding: OnboardingState::default(),
            bulk_selection: BulkSelection::default(),
            project_contexts: BTreeMap::new(),
            failure_inbox_preferences: BTreeMap::new(),
            inspector_auto_open_suppressed: false,
            appearance: AppearancePreferencesV1::default(),
            focus: super::FocusRegion::Editor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::QueryScope;

    #[test]
    fn named_time_scope_maps_to_a_bounded_run_window() {
        let now = 10 * 24 * 60 * 60 * 1_000_000_000_u64;
        let mut scope = QueryScope::default();
        assert_eq!(scope.started_after_unix_nano(now), None);
        scope.time_range = Some("last_day".into());
        assert_eq!(
            scope.started_after_unix_nano(now),
            Some(now - 24 * 60 * 60 * 1_000_000_000)
        );
        scope.time_range = Some("future_schema_value".into());
        assert_eq!(scope.started_after_unix_nano(now), None);
    }
}
