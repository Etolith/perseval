use gpui::{Entity, FontWeight, Role, Toggled, Window, div, prelude::*, px};

use crate::components::{TextInput, button};
use crate::design::{ControlSize, Theme};

pub(super) fn section(title: &str, description: &str) -> gpui::Div {
    div()
        .mt_6()
        .p_5()
        .rounded(px(7.))
        .border_1()
        .border_color(Theme::BORDER)
        .bg(Theme::PANEL)
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_string()),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(Theme::MUTED)
                .child(description.to_string()),
        )
}

pub(super) fn setting_row(label: &str, value: String, stacked: bool) -> gpui::Div {
    div()
        .mt_4()
        .pt_4()
        .border_t_1()
        .border_color(Theme::BORDER)
        .flex()
        .when(stacked, |row| row.flex_col().gap_2())
        .when(!stacked, |row| row.items_start().justify_between().gap_5())
        .child(
            div()
                .w(px(190.))
                .when(stacked, |label| label.w_full())
                .flex_none()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .child(label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .when(!stacked, |value| value.text_right())
                .text_xs()
                .text_color(Theme::MUTED)
                .child(value),
        )
}

/// A review row intentionally shows the complete value. It is used for
/// immutable artifacts that a human must be able to read before activation;
/// ordinary status rows stay compact through `setting_row`.
pub(super) fn review_row(
    label: &str,
    value: String,
    detail: String,
    stacked: bool,
) -> impl IntoElement {
    let accessible_label = format!("{label}. {value}. {detail}");
    div()
        .id(accessible_label.clone())
        .role(Role::Group)
        .aria_label(accessible_label)
        .mt_4()
        .pt_4()
        .border_t_1()
        .border_color(Theme::BORDER)
        .flex()
        .when(stacked, |row| row.flex_col().gap_2())
        .when(!stacked, |row| row.items_start().gap_5())
        .child(
            div()
                .w(px(190.))
                .when(stacked, |label| label.w_full())
                .flex_none()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .child(label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .text_left()
                .child(div().text_xs().text_color(Theme::TEXT).child(value))
                .child(div().mt_2().text_xs().text_color(Theme::DIM).child(detail)),
        )
}

pub(super) fn editable_row(
    label: &str,
    detail: &str,
    input: Entity<TextInput>,
    stacked: bool,
) -> gpui::Div {
    div()
        .mt_4()
        .pt_4()
        .border_t_1()
        .border_color(Theme::BORDER)
        .flex()
        .when(stacked, |row| row.flex_col().gap_3())
        .when(!stacked, |row| row.items_center().justify_between().gap_5())
        .child(
            div()
                .w(px(250.))
                .when(stacked, |label| label.w_full())
                .flex_none()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(Theme::MUTED)
                        .child(detail.to_string()),
                ),
        )
        .child(
            div()
                .min_w_0()
                .when(stacked, |field| field.w_full())
                .when(!stacked, |field| field.w(px(350.)))
                .child(input),
        )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn switch_row<F>(
    label: &str,
    detail: &str,
    value: &str,
    selected: bool,
    id: &'static str,
    on_click: F,
    stacked: bool,
) -> gpui::Div
where
    F: Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
{
    div()
        .mt_4()
        .pt_4()
        .border_t_1()
        .border_color(Theme::BORDER)
        .flex()
        .when(stacked, |row| row.flex_col().gap_3())
        .when(!stacked, |row| row.items_center().justify_between().gap_5())
        .child(
            div()
                .w(px(250.))
                .when(stacked, |label| label.w_full())
                .flex_none()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(Theme::MUTED)
                        .child(detail.to_string()),
                ),
        )
        .child(
            button(value, selected)
                .id(id)
                .role(Role::Switch)
                .aria_label(format!("{label}: {value}"))
                .aria_toggled(if selected {
                    Toggled::True
                } else {
                    Toggled::False
                })
                .on_click(on_click),
        )
}

pub(super) fn action_button<F>(
    label: &str,
    aria_label: &str,
    enabled: bool,
    primary: bool,
    on_click: F,
) -> gpui::Stateful<gpui::Div>
where
    F: Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
{
    div()
        .id(aria_label.to_ascii_lowercase().replace(' ', "-"))
        .role(Role::Button)
        .aria_label(aria_label.to_string())
        .tab_index(if enabled { 0 } else { -1 })
        .h(px(ControlSize::DEFAULT))
        .px_4()
        .flex()
        .items_center()
        .rounded(px(5.))
        .border_1()
        .border_color(if primary && enabled {
            Theme::CYAN
        } else {
            Theme::BORDER
        })
        .bg(if primary && enabled {
            Theme::CYAN
        } else {
            Theme::PANEL_ALT
        })
        .text_color(if primary && enabled {
            Theme::TEXT_ON_ACCENT
        } else if enabled {
            Theme::TEXT
        } else {
            Theme::DIM
        })
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .focus_visible(|style| style.border_2().border_color(Theme::CYAN))
                .on_click(on_click)
        })
        .child(label.to_string())
}

pub(super) fn notice(title: &str, detail: String, tint: gpui::Rgba) -> gpui::Div {
    div()
        .mt_4()
        .p_3()
        .rounded(px(5.))
        .border_1()
        .border_color(tint)
        .bg(Theme::INSET_SURFACE)
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(tint)
                .child(title.to_string()),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(Theme::MUTED)
                .child(detail),
        )
}
