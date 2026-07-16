use std::ops::Range;

use gpui::{
    AccessibleAction, App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable, GlobalElementId,
    KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, Role, ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle,
    Window, accesskit::ActionData, actions, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::design::{ControlSize, Theme};

actions!(
    perseval_text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        ClearOrDismiss,
    ]
);

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("PersevalTextInput")),
        KeyBinding::new("delete", Delete, Some("PersevalTextInput")),
        KeyBinding::new("left", Left, Some("PersevalTextInput")),
        KeyBinding::new("right", Right, Some("PersevalTextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("PersevalTextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("PersevalTextInput")),
        KeyBinding::new("cmd-a", SelectAll, Some("PersevalTextInput")),
        KeyBinding::new("cmd-v", Paste, Some("PersevalTextInput")),
        KeyBinding::new("cmd-c", Copy, Some("PersevalTextInput")),
        KeyBinding::new("cmd-x", Cut, Some("PersevalTextInput")),
        KeyBinding::new("home", Home, Some("PersevalTextInput")),
        KeyBinding::new("end", End, Some("PersevalTextInput")),
        KeyBinding::new("escape", ClearOrDismiss, Some("PersevalTextInput")),
        KeyBinding::new(
            "ctrl-cmd-space",
            ShowCharacterPalette,
            Some("PersevalTextInput"),
        ),
    ]);
}

pub(crate) struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    last_scroll_x: Pixels,
    is_selecting: bool,
    maximum_bytes: usize,
}

impl TextInput {
    pub(crate) fn new(
        placeholder: impl Into<SharedString>,
        maximum_bytes: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle().tab_index(0).tab_stop(true),
            content: "".into(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            last_scroll_x: px(0.),
            is_selecting: false,
            maximum_bytes,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.content
    }

    pub(crate) fn set_text(&mut self, value: impl Into<SharedString>, cx: &mut Context<Self>) {
        let value = value.into();
        self.content = truncate_boundary(&value, self.maximum_bytes)
            .to_string()
            .into();
        self.selected_range = self.content.len()..self.content.len();
        self.marked_range = None;
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &single_line_text(&text), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        self.copy(&Copy, window, cx);
        if !self.selected_range.is_empty() {
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn clear_or_dismiss(&mut self, _: &ClearOrDismiss, _: &mut Window, cx: &mut Context<Self>) {
        if self.content.is_empty() {
            cx.propagate();
        } else {
            self.content = "".into();
            self.selected_range = 0..0;
            self.selection_reversed = false;
            self.marked_range = None;
            cx.notify();
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.x <= bounds.left() {
            return 0;
        }
        if position.x >= bounds.right() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left() + self.last_scroll_x)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        offset_from_utf16(&self.content, offset)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let offset = floor_char_boundary(&self.content, offset.min(self.content.len()));
        self.content[..offset].encode_utf16().count()
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn replace(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) -> Range<usize> {
        let range = normalize_range(&self.content, range);
        let available = self
            .maximum_bytes
            .saturating_sub(self.content.len().saturating_sub(range.len()));
        let text = truncate_boundary(text, available);
        let cursor = range.start + text.len();
        self.content = format!(
            "{}{}{}",
            &self.content[..range.start],
            text,
            &self.content[range.end..]
        )
        .into();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
        range.start..cursor
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());
        self.replace(range, new_text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selection_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());
        let inserted = self.replace(range, new_text, cx);
        if !inserted.is_empty() {
            self.marked_range = Some(inserted.clone());
        }
        if let Some(selection) = new_selection_utf16 {
            let inserted_text = &self.content[inserted.clone()];
            self.selected_range = marked_selection(inserted.start, inserted_text, selection);
        }
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + line.x_for_index(range.start) - self.last_scroll_x,
                bounds.top(),
            ),
            point(
                bounds.left() + line.x_for_index(range.end) - self.last_scroll_x,
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line = self.last_layout.as_ref()?;
        let index = line.index_for_x(point.x - bounds.left() + self.last_scroll_x)?;
        Some(self.offset_to_utf16(index))
    }
}

struct TextElement(Entity<TextInput>);

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    scroll_x: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.0.read(cx);
        let selected_range = normalize_range(&input.content, input.selected_range.clone());
        let cursor = if input.content.is_empty() {
            0
        } else {
            floor_char_boundary(
                &input.content,
                input.cursor_offset().min(input.content.len()),
            )
        };
        let style = window.text_style();
        let (display_text, text_color) = if input.content.is_empty() {
            (input.placeholder.clone(), Theme::DIM.into())
        } else {
            (input.content.clone(), style.color)
        };
        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked) = input
            .marked_range
            .as_ref()
            .map(|range| normalize_range(&input.content, range.clone()))
            .filter(|range| !input.content.is_empty() && !range.is_empty())
        {
            vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);
        let cursor_x = line.x_for_index(cursor);
        let scroll_x = if cursor_x > bounds.size.width {
            cursor_x - bounds.size.width + px(1.)
        } else {
            px(0.)
        };
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_x - scroll_x, bounds.top()),
                        size(px(1.), bounds.size.height),
                    ),
                    Theme::FOCUS_RING,
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected_range.start) - scroll_x,
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected_range.end) - scroll_x,
                            bounds.bottom(),
                        ),
                    ),
                    Theme::ROW_SELECTED,
                )),
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            cursor,
            selection,
            scroll_x,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.0.read(cx).focus_handle.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.0.clone()), cx);
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        let line = prepaint.line.take().expect("prepaint shaped a line");
        let _ = line.paint(
            point(bounds.origin.x - prepaint.scroll_x, bounds.origin.y),
            window.line_height(),
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        );
        if focus.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.0.update(cx, |input, _| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
            input.last_scroll_x = prepaint.scroll_x;
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        div()
            .id(("text-input", cx.entity_id()))
            .role(Role::TextInput)
            .aria_label(self.placeholder.clone())
            .focus_visible(|style| style.border_2().border_color(Theme::FOCUS_RING))
            .h(px(ControlSize::DEFAULT))
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .px_3()
            .flex()
            .items_center()
            .key_context("PersevalTextInput")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_a11y_action(AccessibleAction::SetValue, move |data, _, cx| {
                let Some(ActionData::Value(value)) = data else {
                    return;
                };
                entity.update(cx, |input, cx| input.set_text(value.as_ref(), cx));
            })
            .rounded(px(5.))
            .border_1()
            .border_color(if self.focus_handle.is_focused(window) {
                Theme::FOCUS_RING
            } else {
                Theme::BORDER
            })
            .bg(Theme::TOOLBAR_SURFACE)
            .text_sm()
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::clear_or_dismiss))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextElement(cx.entity()))
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn truncate_boundary(value: &str, maximum_bytes: usize) -> &str {
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut end = maximum_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn floor_char_boundary(value: &str, offset: usize) -> usize {
    let mut offset = offset.min(value.len());
    while !value.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn normalize_range(value: &str, range: Range<usize>) -> Range<usize> {
    let start = floor_char_boundary(value, range.start);
    let end = floor_char_boundary(value, range.end).max(start);
    start..end
}

fn offset_from_utf16(value: &str, offset: usize) -> usize {
    let mut utf8 = 0;
    let mut utf16 = 0;
    for character in value.chars() {
        let next_utf8 = utf8 + character.len_utf8();
        let next_utf16 = utf16 + character.len_utf16();
        if offset < next_utf16 {
            return utf8;
        }
        if offset == next_utf16 {
            return next_utf8;
        }
        utf8 = next_utf8;
        utf16 = next_utf16;
    }
    value.len()
}

fn marked_selection(
    inserted_start: usize,
    inserted_text: &str,
    selection_utf16: Range<usize>,
) -> Range<usize> {
    inserted_start + offset_from_utf16(inserted_text, selection_utf16.start)
        ..inserted_start + offset_from_utf16(inserted_text, selection_utf16.end)
}

fn single_line_text(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, ClipboardItem, EntityInputHandler, TestAppContext};

    use super::{
        TextInput, init, marked_selection, normalize_range, offset_from_utf16, single_line_text,
        truncate_boundary,
    };

    #[test]
    fn truncation_preserves_utf8_boundaries() {
        assert_eq!(truncate_boundary("aéz", 2), "a");
        assert_eq!(truncate_boundary("aéz", 3), "aé");
    }

    #[test]
    fn platform_ranges_are_clamped_to_valid_utf8() {
        assert_eq!(normalize_range("", 19..19), 0..0);
        assert_eq!(normalize_range("aéz", 2..99), 1..4);
        assert_eq!(offset_from_utf16("🚀a", 1), 0);
        assert_eq!(offset_from_utf16("🚀a", 2), 4);
        assert_eq!(offset_from_utf16("🚀a", 99), 5);
    }

    #[test]
    fn multiline_clipboard_text_is_made_safe_for_single_line_filters() {
        assert_eq!(
            single_line_text("alpha\r\nbeta\ngamma"),
            "alpha  beta gamma"
        );
    }

    #[test]
    fn ime_selection_offsets_are_relative_to_the_inserted_utf16_text() {
        assert_eq!(marked_selection(4, "🚀é", 0..2), 4..8);
        assert_eq!(marked_selection(4, "🚀é", 2..3), 8..10);
        assert_eq!(marked_selection(4, "🚀é", 99..99), 10..10);
    }

    #[gpui::test]
    fn paste_replaces_selection_and_escape_clears_before_parent_shortcuts(cx: &mut TestAppContext) {
        cx.update(init);
        let window = cx.add_window(|window, cx| {
            let input = TextInput::new("Filter", 64, cx);
            input.focus_handle.focus(window, cx);
            input
        });
        cx.run_until_parked();
        cx.simulate_input(window.into(), "alpha");
        cx.simulate_keystrokes(window.into(), "cmd-a");
        cx.write_to_clipboard(ClipboardItem::new_string("βeta\nline".into()));
        cx.simulate_keystrokes(window.into(), "cmd-v");
        let text = cx
            .read_window(&window, |input, cx| input.read(cx).text().to_owned())
            .expect("read text input");
        assert_eq!(text, "βeta line");

        cx.simulate_keystrokes(window.into(), "escape");
        let text = cx
            .read_window(&window, |input, cx| input.read(cx).text().to_owned())
            .expect("read cleared input");
        assert!(text.is_empty());
    }

    #[gpui::test]
    fn composition_marks_text_and_unmark_preserves_the_committed_value(cx: &mut TestAppContext) {
        let window = cx.add_window(|window, cx| {
            let mut input = TextInput::new("Filter", 64, cx);
            input.set_text("go ", cx);
            input.focus_handle.focus(window, cx);
            input
        });
        window
            .update(cx, |input, window, cx| {
                input.replace_and_mark_text_in_range(None, "🚀", Some(2..2), window, cx);
                assert_eq!(input.marked_text_range(window, cx), Some(3..5));
                assert_eq!(
                    input.selected_text_range(false, window, cx).unwrap().range,
                    5..5
                );
                input.unmark_text(window, cx);
            })
            .expect("update input window");
        let text = cx
            .read_window(&window, |input, cx| input.read(cx).text().to_owned())
            .expect("read composed input");
        assert_eq!(text, "go 🚀");
    }
}
