use super::input_element::{TextElement, TextLayoutCache};
use super::input_layout::{Caret, TextLayout};
use super::input_text::{
    line_range_at, normalize_inserted_text, offset_from_utf16_in, offset_to_utf16_in, word_range_at,
};
use super::theme;
use gpui::{
    Action, App, Bounds, Context, CursorStyle, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, Image, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    Role, ScrollEvent, SharedString, StatefulInteractiveElement, Task, UTF16Selection, Window,
    actions, div, point, prelude::*, px,
};
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

pub(super) const LINE_HEIGHT: f32 = 22.;
const SINGLE_LINE_HEIGHT: f32 = 30.;
const MAX_UNDO_STATES: usize = 100;
const UNDO_GROUP_INTERVAL: Duration = Duration::from_millis(750);

const CARET_BLINK_INTERVAL: Duration = Duration::from_millis(530);
const DRAG_SCROLL_INTERVAL: Duration = Duration::from_millis(50);

actions!(
    codex_image_input,
    [
        SelectAll,
        Paste,
        Cut,
        Copy,
        Undo,
        Redo,
        InsertNewline,
        ShowCharacterPalette,
    ]
);

/// A destination relative to the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    LineStart,
    LineEnd,
    DocumentStart,
    DocumentEnd,
}

/// Moves the cursor by a [`Motion`], collapsing any selection.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = codex_image_input, no_json)]
pub struct Move(pub Motion);

/// Extends the selection by a [`Motion`].
#[derive(Clone, PartialEq, Action)]
#[action(namespace = codex_image_input, no_json)]
pub struct Select(pub Motion);

/// Deletes the selection, or from the cursor through a [`Motion`].
#[derive(Clone, PartialEq, Action)]
#[action(namespace = codex_image_input, no_json)]
pub struct Delete(pub Motion);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextInputMode {
    SingleLine,
    AutoGrow { max_lines: usize },
}

impl TextInputMode {
    pub(super) fn is_multiline(self) -> bool {
        !matches!(self, Self::SingleLine)
    }
}

#[derive(Clone)]
pub(super) struct InputSnapshot {
    content: SharedString,
    selected: Range<usize>,
    selection_reversed: bool,
}

/// A selection drag in progress. One begun by a double or triple click
/// extends by whole words or lines around the `anchor` it first selected.
#[derive(Clone)]
pub(super) enum Drag {
    Character,
    Unit {
        range_at: fn(&str, usize) -> Range<usize>,
        anchor: Range<usize>,
    },
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
    /// Which row the cursor sits on when its offset is a soft wrap.
    pub(super) cursor_at_row_end: bool,
    pub(super) marked: Option<Range<usize>>,
    pub(super) last_layout: Option<Arc<TextLayout>>,
    pub(super) layout_cache: Option<TextLayoutCache>,
    pub(super) last_bounds: Option<Bounds<Pixels>>,
    pub(super) measured_visual_lines: usize,
    pub(super) scroll_x: f32,
    pub(super) scroll_y: f32,
    pub(super) vertical_inset: f32,
    pub(super) preferred_x: Option<f32>,
    pub(super) drag: Option<Drag>,
    drag_scroll: Option<Task<()>>,
    pub(super) caret_visible: bool,
    pub(super) caret_blink: Option<Task<()>>,
    pub(super) undo_stack: Vec<InputSnapshot>,
    pub(super) redo_stack: Vec<InputSnapshot>,
    pub(super) history_group: Option<(EditKind, Instant)>,
    /// Caret, content length and viewport height the scroll last followed.
    /// The view only re-follows the caret when one of them changes, so wheel
    /// scrolling can move away from the caret without snapping back.
    pub(super) revealed: Option<RevealKey>,
}

pub(super) type RevealKey = (Caret, usize, u32);

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
            cursor_at_row_end: false,
            marked: None,
            last_layout: None,
            layout_cache: None,
            last_bounds: None,
            measured_visual_lines: 1,
            scroll_x: 0.,
            scroll_y: 0.,
            vertical_inset: 0.,
            preferred_x: None,
            drag: None,
            drag_scroll: None,
            caret_visible: true,
            caret_blink: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            history_group: None,
            revealed: None,
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
        self.cursor_at_row_end = false;
        self.marked = None;
        self.reset_layout_state();
        self.clear_history();
        cx.notify();
    }

    pub fn select_content(&mut self, cx: &mut Context<Self>) {
        self.set_selection(0..self.content.len(), false, false, cx);
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
        self.layout_cache = None;
        self.last_bounds = None;
        self.measured_visual_lines = 1;
        self.scroll_x = 0.;
        self.scroll_y = 0.;
        self.vertical_inset = 0.;
        self.preferred_x = None;
        self.drag = None;
        self.drag_scroll = None;
        self.revealed = None;
    }

    fn clear_history(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.history_group = None;
    }

    pub(super) fn viewport_height(&self) -> f32 {
        match self.mode {
            TextInputMode::SingleLine => SINGLE_LINE_HEIGHT,
            TextInputMode::AutoGrow { max_lines } => {
                let lines = self.measured_visual_lines.clamp(1, max_lines.max(1));
                (lines as f32 * LINE_HEIGHT).max(SINGLE_LINE_HEIGHT)
            }
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
        self.cursor_at_row_end = false;
        self.marked = None;
        self.preferred_x = None;
        self.revealed = None;
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

    pub(super) fn caret(&self) -> Caret {
        Caret {
            offset: self.cursor(),
            row_end: self.cursor_at_row_end,
        }
    }

    /// Every selection change that is not an edit goes through here. Nothing
    /// repaints when the selection is already there, which a held drag ticking
    /// against the end of the text relies on.
    fn set_selection(
        &mut self,
        selected: Range<usize>,
        reversed: bool,
        row_end: bool,
        cx: &mut Context<Self>,
    ) {
        self.preferred_x = None;
        self.break_history_group();
        let reversed = reversed && !selected.is_empty();
        if self.marked.is_none()
            && selected == self.selected
            && reversed == self.selection_reversed
            && row_end == self.cursor_at_row_end
        {
            return;
        }
        self.selected = selected;
        self.selection_reversed = reversed;
        self.cursor_at_row_end = row_end;
        self.marked = None;
        cx.notify();
    }

    pub(super) fn move_to(&mut self, caret: Caret, cx: &mut Context<Self>) {
        let offset = caret.offset.min(self.content.len());
        self.set_selection(offset..offset, false, caret.row_end, cx);
    }

    pub(super) fn select_to(&mut self, caret: Caret, cx: &mut Context<Self>) {
        let anchor = if self.selection_reversed {
            self.selected.end
        } else {
            self.selected.start
        };
        let head = caret.offset.min(self.content.len());
        self.set_selection(
            anchor.min(head)..anchor.max(head),
            head < anchor,
            caret.row_end,
            cx,
        );
    }

    /// Replaces `range` with `new_text`, normalized for the mode, and leaves
    /// the cursor after it. Returns where the inserted text now sits.
    pub(super) fn replace(
        &mut self,
        range: Range<usize>,
        new_text: &str,
        kind: EditKind,
        cx: &mut Context<Self>,
    ) -> Range<usize> {
        let normalized = normalize_inserted_text(new_text, self.mode.is_multiline());
        let inserted = range.start..range.start + normalized.len();
        if !(range.is_empty() && normalized.is_empty()) {
            let before = self.snapshot();
            let mut content =
                String::with_capacity(self.content.len() - range.len() + normalized.len());
            content.push_str(&self.content[..range.start]);
            content.push_str(&normalized);
            content.push_str(&self.content[range.end..]);
            self.content = content.into();
            self.record_edit(before, kind);
        }
        self.selected = inserted.end..inserted.end;
        self.selection_reversed = false;
        self.cursor_at_row_end = false;
        self.marked = None;
        self.preferred_x = None;
        cx.notify();
        inserted
    }

    /// Shows the caret and restarts its blink, so it stays solid while it is
    /// being moved. The timer ends itself once the caret stops being drawn.
    pub(super) fn restart_caret_blink(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.caret_visible = true;
        self.caret_blink = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(CARET_BLINK_INTERVAL).await;
                let blinking = this.update_in(cx, |input, window, cx| {
                    let blinking = input.draws_caret(window);
                    if blinking {
                        input.caret_visible = !input.caret_visible;
                        cx.notify();
                    } else {
                        input.caret_blink = None;
                    }
                    blinking
                });
                if !blinking.unwrap_or(false) {
                    break;
                }
            }
        }));
    }

    pub(super) fn draws_caret(&self, window: &Window) -> bool {
        self.selected.is_empty() && self.focus.is_focused(window) && window.is_window_active()
    }

    pub(super) fn caret_for_point(&self, position: Point<Pixels>) -> Caret {
        let (Some(bounds), Some(layout)) = (&self.last_bounds, &self.last_layout) else {
            return Caret::at(0);
        };
        let mut y = position.y - bounds.top() - px(self.vertical_inset) + px(self.scroll_y);
        // Inside the editor every point belongs to a line, including the
        // padding around text shorter than the viewport.
        if position.y >= bounds.top() && position.y < bounds.bottom() {
            y = y
                .max(px(0.))
                .min((layout.total_height - px(1.)).max(px(0.)));
        }
        layout.caret_for_position(point(position.x - bounds.left() + px(self.scroll_x), y))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        let caret = self.caret_for_point(event.position);
        let range_at: Option<fn(&str, usize) -> Range<usize>> = match event.click_count {
            _ if event.modifiers.shift => None,
            0 | 1 => None,
            2 => Some(word_range_at),
            _ => Some(line_range_at),
        };
        self.drag = Some(if let Some(range_at) = range_at {
            let anchor = range_at(&self.content, caret.offset);
            self.set_selection(anchor.clone(), false, false, cx);
            Drag::Unit { range_at, anchor }
        } else {
            if event.modifiers.shift {
                self.select_to(caret, cx);
            } else {
                self.move_to(caret, cx);
            }
            Drag::Character
        });
    }

    fn drag_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let caret = self.caret_for_point(position);
        match self.drag.clone() {
            Some(Drag::Character) => self.select_to(caret, cx),
            Some(Drag::Unit { range_at, anchor }) => {
                let unit = range_at(&self.content, caret.offset);
                self.set_selection(
                    unit.start.min(anchor.start)..unit.end.max(anchor.end),
                    unit.start < anchor.start,
                    false,
                    cx,
                );
            }
            None => {}
        }
    }

    /// Called for every mouse move in the window, so a drag keeps selecting
    /// after the pointer leaves the editor. While it is held outside, a timer
    /// keeps extending the selection and the view scrolls after the cursor.
    pub(super) fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !event.dragging() {
            self.end_drag();
            return;
        }
        cx.stop_propagation();
        let position = event.position;
        self.drag_to(position, cx);
        let outside = self
            .last_bounds
            .is_some_and(|bounds| !bounds.contains(&position));
        self.drag_scroll = outside.then(|| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(DRAG_SCROLL_INTERVAL).await;
                    if this
                        .update(cx, |input, cx| input.drag_to(position, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
        });
    }

    fn end_drag(&mut self) {
        self.drag = None;
        self.drag_scroll = None;
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.drag.is_some() {
            cx.stop_propagation();
        }
        self.end_drag();
    }

    fn scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (Some(bounds), Some(layout)) = (&self.last_bounds, &self.last_layout) else {
            return;
        };
        let delta = event.delta.pixel_delta(px(LINE_HEIGHT));
        let (scroll, delta, overflow) = if self.mode.is_multiline() {
            (
                &mut self.scroll_y,
                delta.y,
                layout.total_height - bounds.size.height,
            )
        } else {
            (
                &mut self.scroll_x,
                delta.x,
                layout.max_width - bounds.size.width,
            )
        };
        let next = (*scroll - f32::from(delta)).clamp(0., f32::from(overflow).max(0.));
        if (next - *scroll).abs() > f32::EPSILON {
            *scroll = next;
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        offset_to_utf16_in(&self.content, range.start)..offset_to_utf16_in(&self.content, range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        offset_from_utf16_in(&self.content, range.start)
            ..offset_from_utf16_in(&self.content, range.end)
    }

    /// The range an input-method edit targets: the one it names, else the
    /// text being composed, else the selection.
    fn edit_range(&self, range: Option<Range<usize>>) -> Range<usize> {
        range
            .map(|range| self.range_from_utf16(&range))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone())
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
        let kind = if self.selected.is_empty() && new_text.graphemes(true).count() == 1 {
            EditKind::Typing
        } else {
            EditKind::Replace
        };
        self.replace(self.edit_range(range), new_text, kind, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let composed = self.replace(self.edit_range(range), new_text, EditKind::Typing, cx);
        if let Some(selected) = selected {
            let text = &self.content[composed.clone()];
            self.selected = composed.start + offset_from_utf16_in(text, selected.start)
                ..composed.start + offset_from_utf16_in(text, selected.end);
        }
        self.marked = (!composed.is_empty()).then_some(composed);
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
        let origin = point(
            bounds.left() - px(self.scroll_x),
            bounds.top() + px(self.vertical_inset - self.scroll_y),
        );
        let start = layout.position_for(Caret::at(range.start));
        let end = layout.position_for(Caret {
            offset: range.end,
            row_end: !range.is_empty(),
        });
        Some(Bounds::from_corners(
            origin + start,
            origin + end + point(px(1.), px(LINE_HEIGHT)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(offset_to_utf16_in(
            &self.content,
            self.caret_for_point(position).offset,
        ))
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(("text-input", cx.entity_id()))
            .key_context("CodexImageInput")
            .track_focus(&self.focus)
            .role(Role::TextInput)
            .aria_label(self.placeholder.clone())
            .aria_placeholder(self.placeholder.clone())
            .aria_value(self.content.clone())
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::move_cursor))
            .on_action(cx.listener(Self::select))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::insert_newline))
            .on_action(cx.listener(Self::show_character_palette))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll(cx.listener(Self::scroll))
            .w_full()
            .h(px(self.viewport_height()))
            .line_height(px(LINE_HEIGHT))
            .text_size(px(14.))
            .text_color(theme::ink())
            .rounded_md()
            .overflow_hidden()
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

    fn redraw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn editor<'a>(
        text: &str,
        mode: TextInputMode,
        cx: &'a mut TestAppContext,
    ) -> (Entity<TextInput>, &'a mut VisualTestContext) {
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::with_mode("", mode, cx));
        input.update_in(cx, |input, window, cx| {
            input.set_content(text.to_owned(), cx);
            window.focus(&input.focus, cx);
        });
        // The second pass lays the text out at its grown height.
        redraw(cx);
        redraw(cx);
        (input, cx)
    }

    /// A window point inside the glyph that starts at `offset`.
    fn point_in(input: &TextInput, offset: usize) -> Point<Pixels> {
        let bounds = input.last_bounds.unwrap();
        let position = input
            .last_layout
            .as_ref()
            .unwrap()
            .position_for(Caret::at(offset));
        point(
            bounds.left() + position.x + px(1. - input.scroll_x),
            bounds.top() + position.y + px(input.vertical_inset - input.scroll_y + 11.),
        )
    }

    fn press(cx: &mut VisualTestContext, position: Point<Pixels>, click_count: usize) {
        cx.simulate_event(MouseDownEvent {
            position,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count,
            first_mouse: false,
        });
    }

    #[gpui::test]
    fn line_edges_stay_on_their_row_at_a_soft_wrap(cx: &mut TestAppContext) {
        let (input, cx) = editor(
            &"lorem ".repeat(600),
            TextInputMode::AutoGrow { max_lines: 12 },
            cx,
        );
        input.update_in(cx, |input, window, cx| {
            let layout = input.last_layout.clone().unwrap();
            assert!(layout.visual_line_count >= 3);
            let second_row = point(px(0.), px(LINE_HEIGHT * 1.5));
            let row_start = layout.caret_for_position(second_row).offset;

            input.move_to(Caret::at(row_start + 3), cx);
            input.move_cursor(&Move(Motion::LineStart), window, cx);
            assert_eq!(input.caret(), Caret::at(row_start));
            assert_eq!(
                layout.position_for(input.caret()),
                point(px(0.), px(LINE_HEIGHT))
            );

            input.move_cursor(&Move(Motion::LineEnd), window, cx);
            let row_end = input.caret();
            let position = layout.position_for(row_end);
            assert!(row_end.row_end && row_end.offset > row_start);
            assert!(position.x > px(0.));
            assert_eq!(position.y, px(LINE_HEIGHT));

            input.move_cursor(&Move(Motion::LineEnd), window, cx);
            assert_eq!(input.caret(), row_end);
            input.move_cursor(&Move(Motion::LineStart), window, cx);
            assert_eq!(input.caret(), Caret::at(row_start));
        });
    }

    #[gpui::test]
    fn a_double_click_drag_selects_whole_words_past_the_editor(cx: &mut TestAppContext) {
        let text = "alpha beta gamma delta";
        let (input, cx) = editor(text, TextInputMode::SingleLine, cx);
        let (beta, gamma, alpha, below) = input.read_with(cx, |input, _| {
            let bounds = input.last_bounds.unwrap();
            (
                point_in(input, 7),
                point_in(input, 13),
                point_in(input, 2),
                point(bounds.right(), bounds.bottom() + px(200.)),
            )
        });
        let selection =
            |cx: &mut VisualTestContext| input.read_with(cx, |input, _| input.selected.clone());

        press(cx, beta, 2);
        assert_eq!(selection(cx), 6..10);
        cx.simulate_mouse_move(gamma, MouseButton::Left, Modifiers::default());
        assert_eq!(selection(cx), 6..16);
        cx.simulate_mouse_move(alpha, MouseButton::Left, Modifiers::default());
        assert_eq!(selection(cx), 0..10);
        assert!(input.read_with(cx, |input, _| input.selection_reversed));
        cx.simulate_mouse_move(below, MouseButton::Left, Modifiers::default());
        assert_eq!(selection(cx), 6..text.len());
        cx.simulate_mouse_up(below, MouseButton::Left, Modifiers::default());
        assert!(input.read_with(cx, |input, _| input.drag.is_none()));
    }

    #[gpui::test]
    fn clicks_in_the_padding_around_a_line_land_on_that_line(cx: &mut TestAppContext) {
        let (input, cx) = editor("hello world", TextInputMode::SingleLine, cx);
        let (top, bottom) = input.read_with(cx, |input, _| {
            let bounds = input.last_bounds.unwrap();
            assert!(input.vertical_inset > 0.);
            (
                point(point_in(input, 6).x, bounds.top() + px(1.)),
                point(point_in(input, 3).x, bounds.bottom() - px(1.)),
            )
        });
        cx.simulate_click(top, Modifiers::default());
        assert_eq!(input.read_with(cx, |input, _| input.cursor()), 6);
        cx.simulate_click(bottom, Modifiers::default());
        assert_eq!(input.read_with(cx, |input, _| input.cursor()), 3);
    }

    #[gpui::test]
    fn one_motion_moves_selects_and_deletes(cx: &mut TestAppContext) {
        let (input, cx) = editor("one two three", TextInputMode::SingleLine, cx);
        input.update_in(cx, |input, window, cx| {
            input.select(&Select(Motion::WordLeft), window, cx);
            assert_eq!(
                (input.selected.clone(), input.selection_reversed),
                (8..13, true)
            );
            input.move_cursor(&Move(Motion::Left), window, cx);
            assert_eq!(input.selected, 8..8);
            input.delete(&Delete(Motion::WordLeft), window, cx);
            assert_eq!((input.content(), input.cursor()), ("one three", 4));
            input.undo(&Undo, window, cx);
            assert_eq!(input.content(), "one two three");
        });
    }

    #[gpui::test]
    fn the_caret_blinks_only_while_it_is_drawn(cx: &mut TestAppContext) {
        let (input, cx) = editor("hello", TextInputMode::SingleLine, cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        redraw(cx);
        let state = |cx: &mut VisualTestContext| {
            input.read_with(cx, |input, _| {
                (input.caret_visible, input.caret_blink.is_some())
            })
        };
        assert_eq!(state(cx), (true, true));
        cx.executor().advance_clock(CARET_BLINK_INTERVAL);
        cx.run_until_parked();
        assert_eq!(state(cx), (false, true));

        input.update_in(cx, |input, window, cx| {
            input.move_cursor(&Move(Motion::Left), window, cx);
        });
        redraw(cx);
        assert_eq!(state(cx), (true, true));

        cx.update(|window, cx| window.blur(cx));
        cx.executor().advance_clock(CARET_BLINK_INTERVAL);
        cx.run_until_parked();
        assert!(!state(cx).1);
    }
}
