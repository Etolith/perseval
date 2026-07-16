use std::collections::BTreeSet;

use super::{EditorId, EditorTabState};

/// Session-local editor navigation. Open editor state remains persisted by
/// `WorkbenchStateV1`; history intentionally does not resurrect stale artifacts
/// after a restart.
#[derive(Debug, Clone, Default)]
pub struct EditorNavigation {
    entries: Vec<EditorId>,
    cursor: Option<usize>,
    closed: Vec<EditorTabState>,
}

impl EditorNavigation {
    pub fn new(active: Option<EditorId>) -> Self {
        let mut navigation = Self::default();
        if let Some(active) = active {
            navigation.record(active);
        }
        navigation
    }

    pub fn record(&mut self, editor: EditorId) {
        if self.current() == Some(&editor) {
            return;
        }
        let next = self.cursor.map_or(0, |cursor| cursor + 1);
        self.entries.truncate(next);
        self.entries.push(editor);
        self.cursor = Some(self.entries.len() - 1);
    }

    pub fn back(&mut self, available: &BTreeSet<EditorId>) -> Option<EditorId> {
        let mut cursor = self.cursor?;
        while cursor > 0 {
            cursor -= 1;
            if available.contains(&self.entries[cursor]) {
                self.cursor = Some(cursor);
                return Some(self.entries[cursor].clone());
            }
        }
        None
    }

    pub fn forward(&mut self, available: &BTreeSet<EditorId>) -> Option<EditorId> {
        let mut cursor = self.cursor?;
        while cursor + 1 < self.entries.len() {
            cursor += 1;
            if available.contains(&self.entries[cursor]) {
                self.cursor = Some(cursor);
                return Some(self.entries[cursor].clone());
            }
        }
        None
    }

    pub fn remember_closed(&mut self, tab: EditorTabState) {
        self.closed.retain(|closed| closed.id != tab.id);
        self.closed.push(tab);
        const MAX_CLOSED_EDITORS: usize = 32;
        if self.closed.len() > MAX_CLOSED_EDITORS {
            self.closed.remove(0);
        }
    }

    pub fn reopen(&mut self) -> Option<EditorTabState> {
        self.closed.pop()
    }

    pub fn can_reopen(&self) -> bool {
        !self.closed.is_empty()
    }

    fn current(&self) -> Option<&EditorId> {
        self.cursor.and_then(|cursor| self.entries.get(cursor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::{EditorResource, EditorTabState};

    fn id(value: &str) -> EditorId {
        EditorId(value.into())
    }

    #[test]
    fn back_forward_and_branching_are_stable() {
        let mut navigation = EditorNavigation::new(Some(id("inbox")));
        navigation.record(id("trace-a"));
        navigation.record(id("trace-b"));
        let available = [id("inbox"), id("trace-a"), id("trace-b")]
            .into_iter()
            .collect();

        assert_eq!(navigation.back(&available), Some(id("trace-a")));
        assert_eq!(navigation.back(&available), Some(id("inbox")));
        assert_eq!(navigation.forward(&available), Some(id("trace-a")));

        navigation.record(id("eval"));
        assert_eq!(navigation.forward(&available), None);
    }

    #[test]
    fn history_skips_closed_editors_and_reopen_is_bounded() {
        let mut navigation = EditorNavigation::new(Some(id("inbox")));
        navigation.record(id("trace"));
        navigation.record(id("eval"));
        let available = [id("inbox"), id("eval")].into_iter().collect();
        assert_eq!(navigation.back(&available), Some(id("inbox")));

        let tab = EditorTabState::new(EditorResource::Runs, true);
        navigation.remember_closed(tab.clone());
        assert!(navigation.can_reopen());
        assert_eq!(navigation.reopen(), Some(tab));
        assert!(!navigation.can_reopen());
    }
}
