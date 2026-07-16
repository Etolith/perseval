use gpui::{AnyElement, Div, FontWeight, div, prelude::*, px};

use crate::design::Theme;

/// Honest editor placeholder used while a feature owns no query state yet.
///
/// Keeping this in the component layer prevents new editors from inventing
/// their own empty-state spacing and makes replacement with real content local
/// to the owning screen module.
pub(crate) fn editor_empty_state(title: &str, detail: &str, action: Option<AnyElement>) -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .max_w(px(560.))
                .p_8()
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .mt_3()
                        .text_sm()
                        .text_color(Theme::MUTED)
                        .child(detail.to_string()),
                )
                .when_some(action, |content, action| {
                    content.child(div().mt_5().flex().child(action))
                }),
        )
}
