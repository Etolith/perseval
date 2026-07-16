mod actions;
mod commands;
mod editor_registry;
mod model;
mod navigation;
mod persistence;
mod state;

pub use actions::{FocusRegion, PaneId, WorkbenchAction};
pub use commands::{CommandDescriptor, WorkbenchCommand, command_descriptor};
pub use editor_registry::{EditorDescriptor, EditorRegistry};
pub use model::WorkbenchModel;
pub use navigation::EditorNavigation;
pub use persistence::{PersistenceError, decode_state, encode_state, load_state, save_state};
pub use state::{
    ActivityId, AppearancePreferencesV1, BulkSelection, EditorId, EditorKind, EditorResource,
    EditorTabState, FailureInboxPreferencesV1, FailureInboxSort, FailureInboxViewV1,
    FullTraceOrigin, OnboardingState, PaneLayout, ProjectContextState, ProjectScope, QueryScope,
    SavedFailureInboxViewV1, TextScale, WorkbenchStateV1,
};
