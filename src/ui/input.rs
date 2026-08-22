use super::input_element::TextElement;
use super::input_layout::TextLayout;
use super::input_text::{
    line_range_at, normalize_inserted_text, offset_from_utf16_in, offset_to_utf16_in, word_range_at,
};
use super::theme;
use gpui::{
    App, Bounds, Context, CursorStyle, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    Image, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Role,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement, UTF16Selection, Window, actions,
    div, point, prelude::*, px,
};
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

pub(super) const LINE_HEIGHT: f32 = 22.;
const SINGLE_LINE_HEIGHT: f32 = 30.;
const MAX_UNDO_STATES: usize = 100;
const UNDO_GROUP_INTERVAL: Duration = Duration::from_millis(750);

actions!(
    codex_image_input,
    [
        Backspace,
        Delete,
        DeleteWordBackward,
        DeleteWordForward,
        DeleteToLineStart,
        DeleteToLineEnd,
        Left,
        Right,
        Up,
        Down,
        WordLeft,
        WordRight,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectWordLeft,
        SelectWordRight,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        DocumentStart,
        DocumentEnd,
        SelectDocumentStart,
        SelectDocumentEnd,
        Paste,
        Cut,
        Copy,
        Undo,
        Redo,
        InsertNewline,
        ShowCharacterPalette,
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextInputMode {
    SingleLine,
    AutoGrow { max_lines: usize },
    FixedMultiline { lines: usize },
}

impl TextInputMode {
    pub(super) fn is_multiline(self) -> bool {
        !matches!(self, Self::SingleLine)
    }

    pub(super) fn viewport_lines(self, measured_lines: usize) -> usize {
        match self {
            Self::SingleLine => 1,
            Self::AutoGrow { max_lines } => measured_lines.clamp(1, max_lines.max(1)),
            Self::FixedMultiline { lines } => lines.max(1),
        }
    }

    pub(super) fn centers_single_line(self) -> bool {
        !matches!(self, Self::FixedMultiline { .. })
    }
}

#[derive(Clone)]
pub(super) struct InputSnapshot {
    content: SharedString,
    selected: Range<usize>,
    selection_reversed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EditKind {
    Typing,
    DeleteBackward,
    DeleteForward,
    Replace,
}

pub struct TextInput {
    pub(super) focus: FocusHandle,
    pub(super) content: SharedString,
    pub(super) placeholder: SharedString,
    pub(super) mode: TextInputMode,
    pub(super) selected: Range<usize>,
    pub(super) selection_reversed: bool,
    pub(super) marked: Option<Range<usize>>,
    pub(super) last_layout: Option<Arc<TextLayout>>,
    pub(super) last_bounds: Option<Bounds<Pixels>>,
    pub(super) measured_visual_lines: usize,
    pub(super) scroll_x: f32,
    pub(super) scroll_y: f32,
    pub(super) vertical_inset: f32,
    pub(super) preferred_x: Option<f32>,
    pub(super) selecting: bool,
    pub(super) undo_stack: Vec<InputSnapshot>,
    pub(super) redo_stack: Vec<InputSnapshot>,
    pub(super) history_group: Option<(EditKind, Instant)>,
}

pub enum TextInputEvent {
    PastedImages(Arc<[Image]>),
    PastedPaths(Vec<std::path::PathBuf>),
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl TextInput {
    pub fn single_line(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self::with_mode(placeholder, TextInputMode::SingleLine, cx)
    }

    pub fn auto_growing(
        placeholder: impl Into<SharedString>,
        max_lines: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_mode(placeholder, TextInputMode::AutoGrow { max_lines }, cx)
    }

    fn with_mode(
        placeholder: impl Into<SharedString>,
        mode: TextInputMode,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus: cx.focus_handle().tab_index(0).tab_stop(true),
            content: "".into(),
            placeholder: placeholder.into(),
            mode,
            selected: 0..0,
            selection_reversed: false,
            marked: None,
            last_layout: None,
            last_bounds: None,
            measured_visual_lines: 1,
            scroll_x: 0.,
            scroll_y: 0.,
            vertical_inset: 0.,
            preferred_x: None,
            selecting: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            history_group: None,
        }
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn set_content(&mut self, value: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = value.into();
        let end = self.content.len();
        self.selected = end..end;
        self.selection_reversed = false;
        self.marked = None;
        self.reset_layout_state();
        self.clear_history();
        cx.notify();
    }

    pub fn set_mode(&mut self, mode: TextInputMode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            self.reset_layout_state();
            cx.notify();
        }
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_content("", cx);
    }

    fn reset_layout_state(&mut self) {
        self.last_layout = None;
        self.last_bounds = None;
        self.measured_visual_lines = 1;
        self.scroll_x = 0.;
        self.scroll_y = 0.;
        self.vertical_inset = 0.;
        self.preferred_x = None;
        self.selecting = false;
    }

    fn clear_history(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.history_group = None;
    }

    pub(super) fn viewport_height(&self) -> f32 {
        match self.mode {
            TextInputMode::SingleLine => SINGLE_LINE_HEIGHT,
            _ => (self.mode.viewport_lines(self.measured_visual_lines).max(1) as f32 * LINE_HEIGHT)
                .max(SINGLE_LINE_HEIGHT),
        }
    }

    pub(super) fn snapshot(&self) -> InputSnapshot {
        InputSnapshot {
            content: self.content.clone(),
            selected: self.selected.clone(),
            selection_reversed: self.selection_reversed,
        }
    }

    pub(super) fn restore(&mut self, snapshot: InputSnapshot) {
        self.content = snapshot.content;
        self.selected = snapshot.selected;
        self.selection_reversed = snapshot.selection_reversed;
        self.marked = None;
        self.preferred_x = None;
        self.scroll_x = 0.;
        self.scroll_y = 0.;
    }

    pub(super) fn record_edit(&mut self, before: InputSnapshot, kind: EditKind) {
        let now = Instant::now();
        let coalesces = kind != EditKind::Replace
            && self.history_group.is_some_and(|(previous, at)| {
                previous == kind && now.saturating_duration_since(at) <= UNDO_GROUP_INTERVAL
            });
        if !coalesces {
            if self.undo_stack.len() == MAX_UNDO_STATES {
                self.undo_stack.remove(0);
            }
            self.undo_stack.push(before);
        }
        self.history_group = (kind != EditKind::Replace).then_some((kind, now));
        self.redo_stack.clear();
    }

    pub(super) fn break_history_group(&mut self) {
        self.history_group = None;
    }

    pub(super) fn cursor(&self) -> usize {
        if self.selection_reversed {
            self.selected.start
        } else {
            self.selected.end
        }
    }

    pub(super) fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.move_to_with_preference(offset, None, cx);
    }

    /// Moves the cursor, remembering the column vertical movement should aim
    /// for so repeated up/down keeps its horizontal place across short lines.
    pub(super) fn move_to_with_preference(
        &mut self,
        offset: usize,
        preferred_x: Option<f32>,
        cx: &mut Context<Self>,
    ) {
        let offset = offset.min(self.content.len());
        self.selected = offset..offset;
        self.selection_reversed = false;
        self.marked = None;
        self.preferred_x = preferred_x;
        self.break_history_group();
        cx.notify();
    }

    pub(super) fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.content.len());
        if self.selection_reversed {
            self.selected.start = offset;
        } else {
            self.selected.end = offset;
        }
        if self.selected.end < self.selected.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected = self.selected.end..self.selected.start;
        }
        self.marked = None;
        self.preferred_x = None;
        self.break_history_group();
        cx.notify();
    }

    pub(super) fn select_to_vertical(
        &mut self,
        offset: usize,
        preferred_x: f32,
        cx: &mut Context<Self>,
    ) {
        self.select_to(offset, cx);
        self.preferred_x = Some(preferred_x);
    }

    pub(super) fn spliced(&self, range: &Range<usize>, insertion: &str) -> SharedString {
        let mut content =
            String::with_capacity(self.content.len() - (range.end - range.start) + insertion.len());
        content.push_str(&self.content[..range.start]);
        content.push_str(insertion);
        content.push_str(&self.content[range.end..]);
        content.into()
    }

    pub(super) fn apply_edit(
        &mut self,
        range: Range<usize>,
        new_text: &str,
        kind: EditKind,
        cx: &mut Context<Self>,
    ) {
        let normalized = normalize_inserted_text(new_text, self.mode.is_multiline());
        if range.is_empty() && normalized.is_empty() {
            return;
        }
        let before = self.snapshot();
        self.content = self.spliced(&range, &normalized);
        let cursor = range.start + normalized.len();
        self.selected = cursor..cursor;
        self.selection_reversed = false;
        self.marked = None;
        self.preferred_x = None;
        self.record_edit(before, kind);
        cx.notify();
    }

    pub(super) fn index_for_position(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(layout)) = (&self.last_bounds, &self.last_layout) else {
            return 0;
        };
        layout.index_for_position(point(
            position.x - bounds.left() + px(self.scroll_x),
            position.y - bounds.top() - px(self.vertical_inset) + px(self.scroll_y),
        ))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        self.selecting = true;
        self.break_history_group();
        let index = self.index_for_position(event.position);
        if event.modifiers.shift {
            self.select_to(index, cx);
            return;
        }
        match event.click_count {
            2 => {
                self.selected = word_range_at(&self.content, index);
                self.selection_reversed = false;
                self.preferred_x = None;
                cx.notify();
            }
            count if count >= 3 => {
                self.selected = line_range_at(&self.content, index);
                self.selection_reversed = false;
                self.preferred_x = None;
                cx.notify();
            }
            _ => self.move_to(index, cx),
        }
    }

    fn auto_scroll_for_drag(&mut self, position: Point<Pixels>) {
        let (Some(bounds), Some(layout)) = (&self.last_bounds, &self.last_layout) else {
            return;
        };
        if self.mode.is_multiline() {
            let max_scroll =
                (f32::from(layout.total_height) - f32::from(bounds.size.height)).max(0.);
            if position.y < bounds.top() {
                self.scroll_y = (self.scroll_y - LINE_HEIGHT).max(0.);
            } else if position.y > bounds.bottom() {
                self.scroll_y = (self.scroll_y + LINE_HEIGHT).min(max_scroll);
            }
        } else {
            let max_scroll = (f32::from(layout.max_width) - f32::from(bounds.size.width)).max(0.);
            if position.x < bounds.left() {
                self.scroll_x = (self.scroll_x - 24.).max(0.);
            } else if position.x > bounds.right() {
                self.scroll_x = (self.scroll_x + 24.).min(max_scroll);
            }
        }
    }

    pub(super) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selecting && event.dragging() {
            cx.stop_propagation();
            self.auto_scroll_for_drag(event.position);
            self.select_to(self.index_for_position(event.position), cx);
        }
    }

    pub(super) fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            cx.stop_propagation();
        }
        self.selecting = false;
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.mode.is_multiline() {
            return;
        }
        let (Some(bounds), Some(layout)) = (&self.last_bounds, &self.last_layout) else {
            return;
        };
        let max_scroll = (f32::from(layout.total_height) - f32::from(bounds.size.height)).max(0.);
        if max_scroll <= 0. {
            return;
        }
        let delta = event.delta.pixel_delta(px(LINE_HEIGHT));
        let next = (self.scroll_y - f32::from(delta.y)).clamp(0., max_scroll);
        if (next - self.scroll_y).abs() > f32::EPSILON {
            self.scroll_y = next;
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        offset_from_utf16_in(&self.content, offset)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        offset_to_utf16_in(&self.content, offset)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range);
        actual.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        let kind = if self.selected.is_empty() && new_text.graphemes(true).count() == 1 {
            EditKind::Typing
        } else {
            EditKind::Replace
        };
        self.apply_edit(range, new_text, kind, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        let normalized = normalize_inserted_text(new_text, self.mode.is_multiline());
        let before = self.snapshot();
        self.content = self.spliced(&range, &normalized);
        self.marked =
            (!normalized.is_empty()).then_some(range.start..range.start + normalized.len());
        self.selected = selected
            .map(|selected| {
                range.start + offset_from_utf16_in(&normalized, selected.start)
                    ..range.start + offset_from_utf16_in(&normalized, selected.end)
            })
            .unwrap_or_else(|| range.start + normalized.len()..range.start + normalized.len());
        self.selection_reversed = false;
        self.preferred_x = None;
        self.record_edit(before, EditKind::Typing);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range);
        let start = layout.position_for_index(range.start);
        let end = layout.position_for_index(range.end);
        Some(Bounds::from_corners(
            point(
                bounds.left() + start.x - px(self.scroll_x),
                bounds.top() + start.y + px(self.vertical_inset - self.scroll_y),
            ),
            point(
                bounds.left() + end.x - px(self.scroll_x) + px(1.),
                bounds.top() + end.y + px(self.vertical_inset - self.scroll_y) + px(LINE_HEIGHT),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.offset_to_utf16(self.index_for_position(position)))
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        div()
            .id(("text-input", cx.entity_id()))
            .key_context("CodexImageInput")
            .track_focus(&self.focus)
            .role(Role::TextInput)
            .aria_label(self.placeholder.clone())
            .aria_placeholder(self.placeholder.clone())
            .aria_value(self.content.clone())
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_backward))
            .on_action(cx.listener(Self::delete_word_forward))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::delete_to_line_end))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::document_start))
            .on_action(cx.listener(Self::document_end))
            .on_action(cx.listener(Self::select_document_start))
            .on_action(cx.listener(Self::select_document_end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::insert_newline))
            .on_action(cx.listener(Self::show_character_palette))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .w_full()
            .h(px(self.viewport_height()))
            .line_height(px(LINE_HEIGHT))
            .text_size(px(14.))
            .text_color(theme::ink())
            .rounded_md()
            .when(focused, |style| style.bg(theme::hover().opacity(0.22)))
            .overflow_hidden()
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
