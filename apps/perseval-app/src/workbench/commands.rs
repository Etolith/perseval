#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkbenchCommand {
    ShowWelcome,
    ShowFailures,
    ShowRuns,
    ShowCompare,
    ShowEvals,
    ShowSources,
    ShowSettings,
    TogglePrimarySidebar,
    ToggleInspector,
    ToggleBottomPanel,
    FocusNextPane,
    FocusPreviousPane,
    ResetLayout,
    Back,
    Forward,
    ReopenClosedEditor,
    ShowCommandPalette,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub shortcut: Option<&'static str>,
}

pub const fn command_descriptor(command_kind: WorkbenchCommand) -> CommandDescriptor {
    match command_kind {
        WorkbenchCommand::ShowWelcome => command("activity.welcome", "Open Getting Started", ""),
        WorkbenchCommand::ShowFailures => command("activity.failures", "Show Failures", "⌘1"),
        WorkbenchCommand::ShowRuns => command("activity.runs", "Show Runs", "⌘2"),
        WorkbenchCommand::ShowCompare => command("activity.compare", "Show Compare", "⌘3"),
        WorkbenchCommand::ShowEvals => command("activity.evals", "Show Evals", "⌘4"),
        WorkbenchCommand::ShowSources => command("activity.sources", "Show Sources", "⌘5"),
        WorkbenchCommand::ShowSettings => command("activity.settings", "Show Settings", "⌘,"),
        WorkbenchCommand::TogglePrimarySidebar => {
            command("pane.primary_sidebar", "Toggle Primary Sidebar", "⌘B")
        }
        WorkbenchCommand::ToggleInspector => command("pane.inspector", "Toggle Inspector", "⌥⌘I"),
        WorkbenchCommand::ToggleBottomPanel => {
            command("pane.bottom_panel", "Toggle Bottom Panel", "⌘J")
        }
        WorkbenchCommand::FocusNextPane => command("focus.next_pane", "Focus Next Pane", "F6"),
        WorkbenchCommand::FocusPreviousPane => {
            command("focus.previous_pane", "Focus Previous Pane", "⇧F6")
        }
        WorkbenchCommand::ResetLayout => command("layout.reset", "Reset Workbench Layout", ""),
        WorkbenchCommand::Back => command("navigation.back", "Back", "⌘["),
        WorkbenchCommand::Forward => command("navigation.forward", "Forward", "⌘]"),
        WorkbenchCommand::ReopenClosedEditor => {
            command("editor.reopen_closed", "Reopen Closed Editor", "⇧⌘T")
        }
        WorkbenchCommand::ShowCommandPalette => {
            command("command_palette.show", "Show Command Palette", "⇧⌘P")
        }
    }
}

const fn command(
    id: &'static str,
    label: &'static str,
    shortcut: &'static str,
) -> CommandDescriptor {
    CommandDescriptor {
        id,
        label,
        shortcut: if shortcut.is_empty() {
            None
        } else {
            Some(shortcut)
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    const COMMANDS: [WorkbenchCommand; 17] = [
        WorkbenchCommand::ShowWelcome,
        WorkbenchCommand::ShowFailures,
        WorkbenchCommand::ShowRuns,
        WorkbenchCommand::ShowCompare,
        WorkbenchCommand::ShowEvals,
        WorkbenchCommand::ShowSources,
        WorkbenchCommand::ShowSettings,
        WorkbenchCommand::TogglePrimarySidebar,
        WorkbenchCommand::ToggleInspector,
        WorkbenchCommand::ToggleBottomPanel,
        WorkbenchCommand::FocusNextPane,
        WorkbenchCommand::FocusPreviousPane,
        WorkbenchCommand::ResetLayout,
        WorkbenchCommand::Back,
        WorkbenchCommand::Forward,
        WorkbenchCommand::ReopenClosedEditor,
        WorkbenchCommand::ShowCommandPalette,
    ];

    #[test]
    fn command_ids_and_declared_shortcuts_do_not_conflict() {
        let mut ids = BTreeSet::new();
        let mut shortcuts = BTreeSet::new();
        for command in COMMANDS {
            let descriptor = command_descriptor(command);
            assert!(
                ids.insert(descriptor.id),
                "duplicate command id {}",
                descriptor.id
            );
            assert!(!descriptor.label.trim().is_empty());
            if let Some(shortcut) = descriptor.shortcut {
                assert!(
                    shortcuts.insert(shortcut),
                    "duplicate command shortcut {shortcut}"
                );
            }
        }
    }
}
