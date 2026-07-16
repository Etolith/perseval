use gpui::{Div, Rgba, div, prelude::*};

use crate::design::{ExecutionRole, Theme};

pub(crate) fn tag(label: &str, tint: Rgba) -> Div {
    div()
        .px_2()
        .py_1()
        .whitespace_nowrap()
        .rounded_sm()
        // Keep the text/background contrast independent of the parent row.
        // A translucent semantic tint can fall below AA when composited over
        // selected or warning surfaces even when the raw token passes.
        .bg(Theme::ELEVATED_SURFACE)
        .text_xs()
        .text_color(tint)
        .child(label.to_string())
}

pub(crate) fn execution_tag(label: &str, role: ExecutionRole) -> Div {
    div()
        .px_2()
        .py_1()
        .whitespace_nowrap()
        .rounded_sm()
        .bg(role.surface())
        .text_xs()
        .text_color(role.tint())
        .child(label.to_string())
}
