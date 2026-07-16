use std::collections::BTreeMap;

use super::{ActivityId, EditorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorDescriptor {
    pub kind: EditorKind,
    pub activity: ActivityId,
    pub title: &'static str,
    pub previewable: bool,
    pub restorable: bool,
}

#[derive(Debug, Clone)]
pub struct EditorRegistry {
    descriptors: BTreeMap<&'static str, EditorDescriptor>,
}

impl Default for EditorRegistry {
    fn default() -> Self {
        let mut registry = Self {
            descriptors: BTreeMap::new(),
        };
        for descriptor in default_descriptors() {
            registry.register(descriptor);
        }
        registry
    }
}

impl EditorRegistry {
    pub fn register(&mut self, descriptor: EditorDescriptor) {
        self.descriptors
            .insert(kind_key(descriptor.kind), descriptor);
    }

    pub fn descriptor(&self, kind: EditorKind) -> Option<&EditorDescriptor> {
        self.descriptors.get(kind_key(kind))
    }

    pub fn len(&self) -> usize {
        self.descriptors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.descriptors.is_empty()
    }
}

fn default_descriptors() -> [EditorDescriptor; 9] {
    [
        descriptor(EditorKind::Welcome, "Welcome", false),
        descriptor(EditorKind::FailureInbox, "Failure Inbox", false),
        descriptor(
            EditorKind::FailureInvestigation,
            "Failure Investigation",
            true,
        ),
        descriptor(EditorKind::FullTrace, "Full Trace", true),
        descriptor(EditorKind::Runs, "Runs", false),
        descriptor(EditorKind::Sources, "Sources", false),
        descriptor(EditorKind::EvalReview, "Eval Review", true),
        descriptor(EditorKind::Compare, "Compare", true),
        descriptor(EditorKind::Settings, "Settings", false),
    ]
}

const fn descriptor(kind: EditorKind, title: &'static str, previewable: bool) -> EditorDescriptor {
    EditorDescriptor {
        kind,
        activity: kind.activity(),
        title,
        previewable,
        restorable: true,
    }
}

const fn kind_key(kind: EditorKind) -> &'static str {
    match kind {
        EditorKind::Welcome => "welcome",
        EditorKind::FailureInbox => "failure_inbox",
        EditorKind::FailureInvestigation => "failure_investigation",
        EditorKind::FullTrace => "full_trace",
        EditorKind::Runs => "runs",
        EditorKind::Sources => "sources",
        EditorKind::EvalReview => "eval_review",
        EditorKind::Compare => "compare",
        EditorKind::Settings => "settings",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_planned_editor_has_one_descriptor() {
        let registry = EditorRegistry::default();
        assert_eq!(registry.len(), 9);
        assert!(registry.descriptor(EditorKind::Compare).is_some());
    }
}
