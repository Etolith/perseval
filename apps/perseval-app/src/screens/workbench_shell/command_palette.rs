use super::*;

impl WorkbenchShell {
    pub(super) fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.project_menu_open = false;
        self.view_menu_open = false;
        self.transient_return_focus = window.focused(cx);
        self.command_palette_open = true;
        self.command_palette_selection = 0;
        self.command_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.command_input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(super) fn dismiss_transient(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette_open {
            self.command_palette_open = false;
            if let Some(return_focus) = self.transient_return_focus.take() {
                return_focus.focus(window, cx);
            } else {
                self.focus_handle.focus(window, cx);
            }
            cx.notify();
            return;
        }
        if self.project_menu_open {
            self.project_menu_open = false;
            cx.notify();
            return;
        }
        if self.view_menu_open {
            self.view_menu_open = false;
            cx.notify();
            return;
        }
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    pub(super) fn move_command_palette_selection(&mut self, offset: isize, cx: &mut Context<Self>) {
        if !self.command_palette_open {
            return;
        }
        let commands = self.filtered_commands(cx);
        if commands.is_empty() {
            self.command_palette_selection = 0;
        } else {
            self.command_palette_selection = self
                .command_palette_selection
                .saturating_add_signed(offset)
                .min(commands.len() - 1);
        }
        cx.notify();
    }

    pub(super) fn accept_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.command_palette_open {
            return;
        }
        if let Some(command) = self
            .filtered_commands(cx)
            .get(self.command_palette_selection)
            .copied()
        {
            self.execute_command(command, window, cx);
        }
    }

    fn execute_command(
        &mut self,
        command: WorkbenchCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command_palette_open = false;
        self.transient_return_focus = None;
        match command {
            WorkbenchCommand::ShowWelcome => {
                self.open_editor(EditorResource::Welcome, true);
                self.persist();
                cx.notify();
            }
            WorkbenchCommand::ShowFailures => self.open_activity(ActivityId::Failures, cx),
            WorkbenchCommand::ShowRuns => self.open_activity(ActivityId::Runs, cx),
            WorkbenchCommand::ShowCompare => self.open_activity(ActivityId::Compare, cx),
            WorkbenchCommand::ShowEvals => self.open_activity(ActivityId::Evals, cx),
            WorkbenchCommand::ShowSources => self.open_activity(ActivityId::Sources, cx),
            WorkbenchCommand::ShowSettings => self.open_activity(ActivityId::Settings, cx),
            WorkbenchCommand::TogglePrimarySidebar => self.toggle_pane(PaneId::PrimarySidebar, cx),
            WorkbenchCommand::ToggleInspector => self.toggle_pane(PaneId::Inspector, cx),
            WorkbenchCommand::ToggleBottomPanel => self.toggle_pane(PaneId::BottomPanel, cx),
            WorkbenchCommand::FocusNextPane => self.focus_adjacent_pane(false, window, cx),
            WorkbenchCommand::FocusPreviousPane => self.focus_adjacent_pane(true, window, cx),
            WorkbenchCommand::ResetLayout => {
                self.model.apply(WorkbenchAction::ResetLayout);
                self.active_support_pane = None;
                self.persist();
                cx.notify();
            }
            WorkbenchCommand::Back => self.navigate_back(cx),
            WorkbenchCommand::Forward => self.navigate_forward(cx),
            WorkbenchCommand::ReopenClosedEditor => self.reopen_closed_editor(cx),
            WorkbenchCommand::ShowCommandPalette => {}
        }
        self.focus_handle.focus(window, cx);
    }

    fn filtered_commands(&self, cx: &App) -> Vec<WorkbenchCommand> {
        let query = self.command_input.read(cx).text().trim().to_lowercase();
        available_commands()
            .into_iter()
            .filter(|command| {
                let descriptor = command_descriptor(*command);
                query.is_empty()
                    || descriptor.label.to_lowercase().contains(&query)
                    || descriptor.id.contains(&query)
            })
            .collect()
    }

    pub(super) fn render_command_palette(&mut self, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let commands = self.filtered_commands(cx);
        self.command_palette_selection = self
            .command_palette_selection
            .min(commands.len().saturating_sub(1));
        let mut results = div()
            .id("command-palette-results")
            .role(Role::ListBox)
            .aria_label("Commands")
            .flex()
            .flex_col()
            .gap_1()
            .p_2();
        if commands.is_empty() {
            results = results.child(
                div()
                    .px_3()
                    .py_4()
                    .text_sm()
                    .text_color(Theme::MUTED)
                    .child("No matching commands"),
            );
        }
        for (index, command) in commands.into_iter().enumerate() {
            let descriptor = command_descriptor(command);
            let selected = index == self.command_palette_selection;
            results = results.child(
                div()
                    .id(("command-palette-result", index))
                    .role(Role::ListBoxOption)
                    .aria_label(descriptor.label)
                    .aria_selected(selected)
                    .tab_index(0)
                    .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                    .h(px(38.))
                    .px_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .rounded(px(5.))
                    .bg(if selected {
                        Theme::PANEL_ALT
                    } else {
                        Theme::PANEL
                    })
                    .text_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(Theme::PANEL_ALT))
                    .child(descriptor.label)
                    .when_some(descriptor.shortcut, |row, shortcut| {
                        row.child(div().text_xs().text_color(Theme::DIM).child(shortcut))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.execute_command(command, window, cx)
                    })),
            );
        }
        div()
            .id("command-palette-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .flex()
            .justify_center()
            .bg(Theme::OVERLAY)
            .child(
                div()
                    .id("command-palette")
                    .role(Role::Dialog)
                    .aria_label("Command Palette")
                    .mt(px(84.))
                    .w(px(560.))
                    .h(px(390.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(Theme::BORDER)
                    .bg(Theme::PANEL)
                    .shadow_lg()
                    .overflow_hidden()
                    .child(
                        div()
                            .key_context("CommandPalette")
                            .p_3()
                            .border_b_1()
                            .border_color(Theme::BORDER)
                            .child(self.command_input.clone()),
                    )
                    .child(results),
            )
    }
}

fn available_commands() -> [WorkbenchCommand; 16] {
    [
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
    ]
}
