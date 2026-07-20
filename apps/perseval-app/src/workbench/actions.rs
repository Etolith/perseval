use serde::{Deserialize, Serialize};

use super::{
    AppearancePreferencesV1, EditorId, EditorResource, FailureInboxPreferencesV1, QueryScope,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneId {
    PrimarySidebar,
    Inspector,
    BottomPanel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusRegion {
    ActivityRail,
    PrimarySidebar,
    EditorTabs,
    Editor,
    Inspector,
    BottomPanel,
    StatusBar,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorkbenchAction {
    OpenEditor {
        resource: EditorResource,
        pinned: bool,
    },
    ActivateEditor(EditorId),
    PinEditor(EditorId),
    UnpinEditor(EditorId),
    CloseEditor(EditorId),
    SetPaneVisible {
        pane: PaneId,
        visible: bool,
    },
    ResizePane {
        pane: PaneId,
        size: f32,
    },
    SetScope(QueryScope),
    SetFocus(FocusRegion),
    ToggleFailureGroup(String),
    ClearBulkSelection,
    SetFailureInboxPreferences {
        scope_key: String,
        preferences: FailureInboxPreferencesV1,
    },
    SetInspectorAutoOpenSuppressed(bool),
    UpdateActiveFullTraceSelection(Option<String>),
    SetAppearance(AppearancePreferencesV1),
    ResetLayout,
}
