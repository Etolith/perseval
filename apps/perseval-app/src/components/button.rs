use gpui::{Div, div, prelude::*, px};

use crate::design::{ControlSize, Theme};

pub(crate) fn button(label: &str, active: bool) -> Div {
    button_state(label, active, true)
}

pub(crate) fn button_state(label: &str, active: bool, enabled: bool) -> Div {
    div()
        .when(enabled, |button| {
            button
                .tab_index(0)
                .focus_visible(|style| style.border_2().border_color(Theme::FOCUS_RING))
        })
        .min_h(px(ControlSize::DEFAULT))
        .py_2()
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(if active && enabled {
            Theme::FOCUS_RING
        } else {
            Theme::BORDER
        })
        .bg(if active && enabled {
            Theme::PRESSED_SURFACE
        } else {
            Theme::SECONDARY_ACTION_SURFACE
        })
        .text_xs()
        .text_color(if enabled { Theme::TEXT } else { Theme::DIM })
        .when(enabled, |button| button.cursor_pointer())
        .when(!enabled, |button| {
            button
                .bg(Theme::DISABLED_SURFACE)
                .cursor_not_allowed()
                .opacity(0.55)
        })
        .child(label.to_string())
}
