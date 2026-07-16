use super::components::*;
use super::*;

impl FailureInbox {
    /// Compact navigation keeps the inbox identifiable and filterable when
    /// large text makes the semantic table collapse into stacked rows.
    pub(super) fn render_compact_group_header(&self, cx: &mut Context<Self>) -> Div {
        div()
            .px_3()
            .py_3()
            .border_b_1()
            .border_color(Theme::BORDER)
            .bg(Theme::BG)
            .child(
                div()
                    .flex()
                    .items_end()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Failure Inbox"),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(Theme::DIM)
                            .child(format!(
                                "{} of {} groups",
                                self.groups.len(),
                                self.group_total
                            )),
                    ),
            )
            .child(
                div()
                    .mt_3()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        button(
                            &match self.active_group_filter_count() {
                                0 => "Filters".into(),
                                count => format!("Filters · {count}"),
                            },
                            self.active_group_filter_count() > 0,
                        )
                        .id("failure-filters-compact")
                        .role(Role::Button)
                        .aria_label(format!(
                            "Open failure filters; {} active",
                            self.active_group_filter_count()
                        ))
                        .aria_expanded(self.open_filter_menu == Some(InboxFilterMenu::Filters))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle_filter_menu(InboxFilterMenu::Filters, cx)
                        })),
                    )
                    .when(self.selected_group_ids.is_empty(), |toolbar| {
                        toolbar.child(
                            button(
                                &format!("Organize · {}", self.current_sort().label()),
                                self.current_sort() != FailureInboxSort::Priority
                                    || self.preferences.active_saved_view_id.is_some(),
                            )
                            .id("organize-failure-inbox-compact")
                            .role(Role::Button)
                            .aria_label("Open failure sorting and saved views")
                            .aria_expanded(self.open_filter_menu == Some(InboxFilterMenu::Organize))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_filter_menu(InboxFilterMenu::Organize, cx)
                            })),
                        )
                    })
                    .when(!self.selected_group_ids.is_empty(), |toolbar| {
                        toolbar.child(
                            button_state(
                                &format!(
                                    "Generate evals · {} groups",
                                    self.selected_group_ids.len()
                                ),
                                true,
                                self.can_generate_eval(),
                            )
                            .id("preview-selected-evals-compact")
                            .role(Role::Button)
                            .aria_label(format!(
                                "Generate eval candidates for {} selected failure groups",
                                self.selected_group_ids.len()
                            ))
                            .when(self.can_generate_eval(), |button| {
                                button.on_click(
                                    cx.listener(|this, _, _, cx| this.preview_selected_groups(cx)),
                                )
                            }),
                        )
                    }),
            )
            .child(
                div()
                    .mt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(self.search_input.clone())),
            )
            .when_some(self.render_open_filter_menu(cx), |header, menu| {
                header.child(menu)
            })
    }
}
