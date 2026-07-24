use gpui::{AnyElement, Div, FontWeight, IntoElement, div, prelude::*, px};

use crate::design::{Geometry, Spacing, Theme};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DataColumn {
    Flexible,
    Fixed(f32),
}

/// Shared geometry for full-width data editors.
///
/// The header, toolbar, column labels, and rows all begin on the same 24 px
/// content axis. Screens provide their own controls and cells, while this
/// component owns the workbench rhythm that must not drift per feature.
pub(crate) fn data_page_header(
    title: &str,
    description: impl IntoElement,
    summary: impl IntoElement,
) -> Div {
    div()
        .px(px(Geometry::PAGE_GUTTER))
        .pt(px(Spacing::LG))
        .pb(px(Spacing::LG))
        .border_b_1()
        .border_color(Theme::BORDER)
        .bg(Theme::APPLICATION_BACKGROUND)
        .child(
            div()
                .flex()
                .items_end()
                .justify_between()
                .gap(px(Spacing::XL))
                .child(
                    div()
                        .min_w_0()
                        .child(
                            div()
                                .text_xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title.to_string()),
                        )
                        .child(
                            div()
                                .mt(px(Spacing::XS))
                                .text_sm()
                                .text_color(Theme::MUTED)
                                .child(description),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(Theme::DIM)
                        .child(summary),
                ),
        )
}

pub(crate) fn data_page_toolbar() -> Div {
    div()
        .mt(px(Spacing::LG))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(Spacing::SM))
}

pub(crate) fn data_table_header(leading_width: f32, columns: Div) -> Div {
    div()
        .h(px(Geometry::TABLE_ROW_COMPACT))
        .px(px(Geometry::PAGE_GUTTER))
        .flex()
        .items_center()
        .gap(px(Spacing::MD))
        .border_b_1()
        .border_color(Theme::BORDER)
        .bg(Theme::TOOLBAR_SURFACE)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(Theme::DIM)
        .child(div().w(px(leading_width)).flex_none())
        .child(columns)
}

pub(crate) fn data_columns(layout: &[DataColumn], cells: Vec<AnyElement>) -> Div {
    debug_assert_eq!(
        layout.len(),
        cells.len(),
        "a data row must provide exactly one cell for each semantic column"
    );
    div()
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(Spacing::MD))
        .children(layout.iter().copied().zip(cells).map(|(width, cell)| {
            match width {
                DataColumn::Flexible => div().flex_1().min_w_0().overflow_hidden().child(cell),
                DataColumn::Fixed(width) => div()
                    .w(px(width))
                    .flex_none()
                    .min_w_0()
                    .overflow_hidden()
                    .child(cell),
            }
        }))
}
