use super::theme;
use gpui::{
    App, Bounds, ClickEvent, ClipboardEntry, ClipboardItem, ContentMask, Context, CursorStyle,
    Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, GlobalElementId, Image, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, Point, Role, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement, Style, TextRun, UTF16Selection, UnderlineStyle, Window,
    WrappedLine, actions, div, fill, point, prelude::*, px, relative, size,
};
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

const LINE_HEIGHT: f32 = 22.;
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
    fn is_multiline(self) -> bool {
        !matches!(self, Self::SingleLine)
    }

    fn viewport_lines(self, measured_lines: usize) -> usize {
        match self {
            Self::SingleLine => 1,
            Self::AutoGrow { max_lines } => measured_lines.clamp(1, max_lines.max(1)),
            Self::FixedMultiline { lines } => lines.max(1),
        }
    }

    fn centers_single_line(self) -> bool {
        !matches!(self, Self::FixedMultiline { .. })
    }
}

#[derive(Clone)]
struct InputSnapshot {
    content: SharedString,
    selected: Range<usize>,
    selection_reversed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKind {
    Typing,
    DeleteBackward,
    DeleteForward,
    Replace,
}

#[derive(Clone)]
struct LayoutLine {
    shaped: Arc<WrappedLine>,
    start: usize,
    end: usize,
    y: Pixels,
    height: Pixels,
    has_newline: bool,
}

#[derive(Clone, Default)]
struct TextLayout {
    lines: Vec<LayoutLine>,
    content_len: usize,
    line_height: Pixels,
    total_height: Pixels,
    max_width: Pixels,
    visual_line_count: usize,
}

impl TextLayout {
    fn new(shaped_lines: Vec<WrappedLine>, content: &str, line_height: Pixels) -> Self {
        let logical_lines: Vec<&str> = content.split('\n').collect();
        let mut lines = Vec::with_capacity(shaped_lines.len());
        let mut start = 0;
        let mut y = px(0.);
        let mut max_width = px(0.);
        let mut visual_line_count = 0;

        for (index, shaped) in shaped_lines.into_iter().enumerate() {
            let logical_len = logical_lines.get(index).map_or(0, |line| line.len());
            let end = start + logical_len;
            let has_newline = index + 1 < logical_lines.len();
            let shaped = Arc::new(shaped);
            let height = shaped.size(line_height).height;
            max_width = max_width.max(shaped.width());
            visual_line_count += shaped.wrap_boundaries().len() + 1;
            lines.push(LayoutLine {
                shaped,
                start,
                end,
                y,
                height,
                has_newline,
            });
            y += height;
            start = end + usize::from(has_newline);
        }

        Self {
            lines,
            content_len: content.len(),
            line_height,
            total_height: y,
            max_width,
            visual_line_count: visual_line_count.max(1),
        }
    }

    fn position_for_index(&self, index: usize) -> Point<Pixels> {
        let index = index.min(self.content_len);
        for line in &self.lines {
            if index <= line.end {
                let local = index.saturating_sub(line.start).min(line.end - line.start);
                let position = line
                    .shaped
                    .position_for_index(local, self.line_height)
                    .unwrap_or_default();
                return point(position.x, line.y + position.y);
            }
        }
        self.lines
            .last()
            .and_then(|line| {
                line.shaped
                    .position_for_index(line.end - line.start, self.line_height)
                    .map(|position| point(position.x, line.y + position.y))
            })
            .unwrap_or_default()
    }

    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        if self.content_len == 0 || self.lines.is_empty() {
            return 0;
        }
        if position.y < px(0.) {
            return 0;
        }
        for line in &self.lines {
            if position.y < line.y + line.height {
                let local_position = point(position.x, position.y - line.y);
                let local = line
                    .shaped
                    .closest_index_for_position(local_position, self.line_height)
                    .unwrap_or_else(|index| index)
                    .min(line.end - line.start);
                return line.start + local;
            }
        }
        self.content_len
    }

    fn visual_line_edge(&self, index: usize, end: bool) -> usize {
        let position = self.position_for_index(index);
        self.index_for_position(point(
            if end { px(1_000_000.) } else { px(0.) },
            position.y + self.line_height / 2.,
        ))
    }
}

pub struct TextInput {
    focus: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    mode: TextInputMode,
    selected: Range<usize>,
    selection_reversed: bool,
    marked: Option<Range<usize>>,
    last_layout: Option<Arc<TextLayout>>,
    last_bounds: Option<Bounds<Pixels>>,
    measured_visual_lines: usize,
    scroll_x: f32,
    scroll_y: f32,
    vertical_inset: f32,
    preferred_x: Option<f32>,
    selecting: bool,
    undo_stack: Vec<InputSnapshot>,
    redo_stack: Vec<InputSnapshot>,
    history_group: Option<(EditKind, Instant)>,
}

pub enum TextInputEvent {
    PastedImages(Vec<Image>),
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

    fn viewport_height(&self) -> f32 {
        match self.mode {
            TextInputMode::SingleLine => SINGLE_LINE_HEIGHT,
            _ => (self.mode.viewport_lines(self.measured_visual_lines).max(1) as f32 * LINE_HEIGHT)
                .max(SINGLE_LINE_HEIGHT),
        }
    }

    fn snapshot(&self) -> InputSnapshot {
        InputSnapshot {
            content: self.content.clone(),
            selected: self.selected.clone(),
            selection_reversed: self.selection_reversed,
        }
    }

    fn restore(&mut self, snapshot: InputSnapshot) {
        self.content = snapshot.content;
        self.selected = snapshot.selected;
        self.selection_reversed = snapshot.selection_reversed;
        self.marked = None;
        self.preferred_x = None;
        self.scroll_x = 0.;
        self.scroll_y = 0.;
    }

    fn record_edit(&mut self, before: InputSnapshot, kind: EditKind) {
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

    fn break_history_group(&mut self) {
        self.history_group = None;
    }

    fn cursor(&self) -> usize {
        if self.selection_reversed {
            self.selected.start
        } else {
            self.selected.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = offset.min(self.content.len());
        self.selected = offset..offset;
        self.selection_reversed = false;
        self.marked = None;
        self.preferred_x = None;
        self.break_history_group();
        cx.notify();
    }

    fn move_to_vertical(&mut self, offset: usize, preferred_x: f32, cx: &mut Context<Self>) {
        let offset = offset.min(self.content.len());
        self.selected = offset..offset;
        self.selection_reversed = false;
        self.marked = None;
        self.preferred_x = Some(preferred_x);
        self.break_history_group();
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
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

    fn select_to_vertical(&mut self, offset: usize, preferred_x: f32, cx: &mut Context<Self>) {
        self.select_to(offset, cx);
        self.preferred_x = Some(preferred_x);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(previous_grapheme_boundary(&self.content, self.cursor()), cx);
        } else {
            self.move_to(self.selected.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(next_grapheme_boundary(&self.content, self.cursor()), cx);
        } else {
            self.move_to(self.selected.end, cx);
        }
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(previous_word_boundary(&self.content, self.cursor()), cx);
        } else {
            self.move_to(self.selected.start, cx);
        }
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(next_word_boundary(&self.content, self.cursor()), cx);
        } else {
            self.move_to(self.selected.end, cx);
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(previous_grapheme_boundary(&self.content, self.cursor()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(next_grapheme_boundary(&self.content, self.cursor()), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(previous_word_boundary(&self.content, self.cursor()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(next_word_boundary(&self.content, self.cursor()), cx);
    }

    fn vertical_target(&self, delta: i32) -> Option<(usize, f32)> {
        let layout = self.last_layout.as_ref()?;
        let position = layout.position_for_index(self.cursor());
        let preferred_x = self.preferred_x.unwrap_or_else(|| f32::from(position.x));
        let target_y = position.y + layout.line_height * delta as f32 + layout.line_height / 2.;
        Some((
            layout.index_for_position(point(px(preferred_x), target_y)),
            preferred_x,
        ))
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((target, preferred_x)) = self.vertical_target(-1) {
            self.move_to_vertical(target, preferred_x, cx);
        } else {
            self.move_to(0, cx);
        }
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((target, preferred_x)) = self.vertical_target(1) {
            self.move_to_vertical(target, preferred_x, cx);
        } else {
            self.move_to(self.content.len(), cx);
        }
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((target, preferred_x)) = self.vertical_target(-1) {
            self.select_to_vertical(target, preferred_x, cx);
        } else {
            self.select_to(0, cx);
        }
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((target, preferred_x)) = self.vertical_target(1) {
            self.select_to_vertical(target, preferred_x, cx);
        } else {
            self.select_to(self.content.len(), cx);
        }
    }

    fn visual_line_edge(&self, end: bool) -> usize {
        self.last_layout
            .as_ref()
            .map(|layout| layout.visual_line_edge(self.cursor(), end))
            .unwrap_or_else(|| logical_line_edge(&self.content, self.cursor(), end))
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.visual_line_edge(false), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.visual_line_edge(true), cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.visual_line_edge(false), cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.visual_line_edge(true), cx);
    }

    fn document_start(&mut self, _: &DocumentStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn document_end(&mut self, _: &DocumentEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn select_document_start(
        &mut self,
        _: &SelectDocumentStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(0, cx);
    }

    fn select_document_end(
        &mut self,
        _: &SelectDocumentEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.content.len(), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = 0..self.content.len();
        self.selection_reversed = false;
        self.preferred_x = None;
        self.break_history_group();
        cx.notify();
    }

    fn apply_edit(
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
        let mut content = String::with_capacity(
            self.content.len() - (range.end - range.start) + normalized.len(),
        );
        content.push_str(&self.content[..range.start]);
        content.push_str(&normalized);
        content.push_str(&self.content[range.end..]);
        self.content = content.into();
        let cursor = range.start + normalized.len();
        self.selected = cursor..cursor;
        self.selection_reversed = false;
        self.marked = None;
        self.preferred_x = None;
        self.record_edit(before, kind);
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        let range = if self.selected.is_empty() {
            let previous = previous_grapheme_boundary(&self.content, self.cursor());
            if previous == self.cursor() {
                window.play_system_bell();
                return;
            }
            previous..self.cursor()
        } else {
            self.selected.clone()
        };
        self.apply_edit(range, "", EditKind::DeleteBackward, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        let range = if self.selected.is_empty() {
            let next = next_grapheme_boundary(&self.content, self.cursor());
            if next == self.cursor() {
                window.play_system_bell();
                return;
            }
            self.cursor()..next
        } else {
            self.selected.clone()
        };
        self.apply_edit(range, "", EditKind::DeleteForward, cx);
    }

    fn delete_word_backward(
        &mut self,
        _: &DeleteWordBackward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = if self.selected.is_empty() {
            let previous = previous_word_boundary(&self.content, self.cursor());
            if previous == self.cursor() {
                window.play_system_bell();
                return;
            }
            previous..self.cursor()
        } else {
            self.selected.clone()
        };
        self.apply_edit(range, "", EditKind::DeleteBackward, cx);
    }

    fn delete_word_forward(
        &mut self,
        _: &DeleteWordForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = if self.selected.is_empty() {
            let next = next_word_boundary(&self.content, self.cursor());
            if next == self.cursor() {
                window.play_system_bell();
                return;
            }
            self.cursor()..next
        } else {
            self.selected.clone()
        };
        self.apply_edit(range, "", EditKind::DeleteForward, cx);
    }

    fn delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = if self.selected.is_empty() {
            self.visual_line_edge(false)..self.cursor()
        } else {
            self.selected.clone()
        };
        if range.is_empty() {
            window.play_system_bell();
        } else {
            self.apply_edit(range, "", EditKind::DeleteBackward, cx);
        }
    }

    fn delete_to_line_end(
        &mut self,
        _: &DeleteToLineEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = if self.selected.is_empty() {
            self.cursor()..self.visual_line_edge(true)
        } else {
            self.selected.clone()
        };
        if range.is_empty() {
            window.play_system_bell();
        } else {
            self.apply_edit(range, "", EditKind::DeleteForward, cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let mut images = Vec::new();
        let mut paths = Vec::new();
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => images.push(image.clone()),
                ClipboardEntry::ExternalPaths(external) => {
                    paths.extend(external.paths().iter().cloned());
                }
                ClipboardEntry::String(_) => {}
            }
        }
        if !images.is_empty() {
            cx.emit(TextInputEvent::PastedImages(images));
        }
        if !paths.is_empty() {
            cx.emit(TextInputEvent::PastedPaths(paths));
        }
        if let Some(text) = item.text() {
            let range = self.selected.clone();
            self.apply_edit(range, &text, EditKind::Replace, cx);
        } else if item.entries().is_empty() {
            window.play_system_bell();
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected.clone()].to_owned(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            window.play_system_bell();
            return;
        }
        self.copy(&Copy, window, cx);
        self.apply_edit(self.selected.clone(), "", EditKind::Replace, cx);
    }

    fn undo(&mut self, _: &Undo, window: &mut Window, cx: &mut Context<Self>) {
        let Some(previous) = self.undo_stack.pop() else {
            window.play_system_bell();
            return;
        };
        let current = self.snapshot();
        self.redo_stack.push(current);
        self.restore(previous);
        self.break_history_group();
        cx.notify();
    }

    fn redo(&mut self, _: &Redo, window: &mut Window, cx: &mut Context<Self>) {
        let Some(next) = self.redo_stack.pop() else {
            window.play_system_bell();
            return;
        };
        let current = self.snapshot();
        self.undo_stack.push(current);
        self.restore(next);
        self.break_history_group();
        cx.notify();
    }

    fn insert_newline(&mut self, _: &InsertNewline, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode.is_multiline() {
            self.apply_edit(self.selected.clone(), "\n", EditKind::Replace, cx);
        } else {
            window.play_system_bell();
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

    fn index_for_position(&self, position: Point<Pixels>) -> usize {
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

    fn click(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
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

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting && event.dragging() {
            cx.stop_propagation();
            self.auto_scroll_for_drag(event.position);
            self.select_to(self.index_for_position(event.position), cx);
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
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
        let mut content = String::with_capacity(
            self.content.len() - (range.end - range.start) + normalized.len(),
        );
        content.push_str(&self.content[..range.start]);
        content.push_str(&normalized);
        content.push_str(&self.content[range.end..]);
        self.content = content.into();
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

struct TextElement {
    input: Entity<TextInput>,
}

struct Prepaint {
    layout: Arc<TextLayout>,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
    origin: Point<Pixels>,
    measured_visual_lines: usize,
    scroll_x: f32,
    scroll_y: f32,
    vertical_inset: f32,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

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
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let input = self.input.read(cx);
        let style = window.text_style();
        let content = input.content.clone();
        let selected = input.selected.clone();
        let cursor_index = input.cursor();
        let marked = input.marked.clone();
        let mode = input.mode;
        let focused = input.focus.is_focused(window);
        let previous_scroll_x = input.scroll_x;
        let previous_scroll_y = input.scroll_y;
        let placeholder = input.placeholder.clone();

        let is_placeholder = content.is_empty();
        let (display_text, color) = if is_placeholder {
            (placeholder, theme::faint())
        } else {
            (content.clone(), style.color)
        };
        let base = TextRun {
            len: display_text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if !is_placeholder {
            if let Some(marked) = &marked {
                vec![
                    TextRun {
                        len: marked.start,
                        ..base.clone()
                    },
                    TextRun {
                        len: marked.end - marked.start,
                        underline: Some(UnderlineStyle {
                            color: Some(color),
                            thickness: px(1.),
                            wavy: false,
                        }),
                        ..base.clone()
                    },
                    TextRun {
                        len: display_text.len() - marked.end,
                        ..base
                    },
                ]
                .into_iter()
                .filter(|run| run.len > 0)
                .collect()
            } else {
                vec![base]
            }
        } else {
            vec![base]
        };
        let shaped = window
            .text_system()
            .shape_text(
                display_text,
                style.font_size.to_pixels(window.rem_size()),
                &runs,
                mode.is_multiline().then_some(bounds.size.width),
                None,
            )
            .unwrap_or_default()
            .into_iter()
            .collect();
        let logical_content = if is_placeholder { "" } else { &content };
        let layout = Arc::new(TextLayout::new(shaped, logical_content, px(LINE_HEIGHT)));
        let measured_visual_lines = layout.visual_line_count;
        let viewport_width = f32::from(bounds.size.width).max(0.);
        let viewport_height = f32::from(bounds.size.height).max(0.);
        let total_height = f32::from(layout.total_height);
        let vertical_inset = if mode.centers_single_line() && total_height < viewport_height {
            (viewport_height - total_height) / 2.
        } else {
            0.
        };

        let caret = layout.position_for_index(cursor_index);
        let mut scroll_x = if mode.is_multiline() {
            0.
        } else {
            previous_scroll_x
        };
        let mut scroll_y = if mode.is_multiline() {
            previous_scroll_y
        } else {
            0.
        };
        if focused {
            if mode.is_multiline() {
                let max_scroll = (total_height - viewport_height).max(0.);
                let caret_top = f32::from(caret.y);
                let caret_bottom = caret_top + LINE_HEIGHT;
                if caret_top < scroll_y {
                    scroll_y = caret_top;
                } else if caret_bottom > scroll_y + viewport_height {
                    scroll_y = caret_bottom - viewport_height;
                }
                scroll_y = scroll_y.clamp(0., max_scroll);
            } else {
                let max_scroll = (f32::from(layout.max_width) - viewport_width).max(0.);
                let caret_x = f32::from(caret.x);
                if caret_x < scroll_x + 2. {
                    scroll_x = (caret_x - 2.).max(0.);
                } else if caret_x > scroll_x + viewport_width - 3. {
                    scroll_x = caret_x - viewport_width + 3.;
                }
                scroll_x = scroll_x.clamp(0., max_scroll);
            }
        }

        let origin = point(
            bounds.left() - px(scroll_x),
            bounds.top() + px(vertical_inset - scroll_y),
        );
        let selections = selection_quads(
            &layout,
            &selected,
            bounds,
            scroll_x,
            scroll_y,
            vertical_inset,
        );
        let cursor = selected.is_empty().then(|| {
            fill(
                Bounds::new(
                    point(origin.x + caret.x, origin.y + caret.y),
                    size(px(1.5), px(LINE_HEIGHT)),
                ),
                theme::accent(),
            )
        });

        Prepaint {
            layout,
            cursor,
            selections,
            origin,
            measured_visual_lines,
            scroll_x,
            scroll_y,
            vertical_inset,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for selection in prepaint.selections.drain(..) {
                window.paint_quad(selection);
            }
            for line in &prepaint.layout.lines {
                let _ = line.shaped.paint(
                    point(prepaint.origin.x, prepaint.origin.y + line.y),
                    px(LINE_HEIGHT),
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            if focus.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });
        self.input.update(cx, |input, cx| {
            let height_changed = input.measured_visual_lines != prepaint.measured_visual_lines;
            input.last_layout = Some(prepaint.layout.clone());
            input.last_bounds = Some(bounds);
            input.measured_visual_lines = prepaint.measured_visual_lines;
            input.scroll_x = prepaint.scroll_x;
            input.scroll_y = prepaint.scroll_y;
            input.vertical_inset = prepaint.vertical_inset;
            if height_changed && matches!(input.mode, TextInputMode::AutoGrow { .. }) {
                cx.notify();
            }
        });
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
            .on_click(cx.listener(Self::click))
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

fn selection_quads(
    layout: &TextLayout,
    selected: &Range<usize>,
    bounds: Bounds<Pixels>,
    scroll_x: f32,
    scroll_y: f32,
    vertical_inset: f32,
) -> Vec<PaintQuad> {
    if selected.is_empty() {
        return Vec::new();
    }
    let mut quads = Vec::new();
    for line in &layout.lines {
        if selected.end <= line.start || selected.start > line.end {
            continue;
        }
        let local_start = selected
            .start
            .saturating_sub(line.start)
            .min(line.end - line.start);
        let local_end = selected
            .end
            .saturating_sub(line.start)
            .min(line.end - line.start);
        let selects_newline =
            line.has_newline && selected.start <= line.end && selected.end > line.end;
        if local_start == local_end && !selects_newline {
            continue;
        }
        let start = line
            .shaped
            .position_for_index(local_start, layout.line_height)
            .unwrap_or_default();
        let end = line
            .shaped
            .position_for_index(local_end, layout.line_height)
            .unwrap_or(start);
        let first_row = (start.y / layout.line_height) as usize;
        let last_row = (end.y / layout.line_height) as usize;
        for row in first_row..=last_row {
            let left = if row == first_row {
                f32::from(start.x)
            } else {
                0.
            };
            let right = if row == last_row && !selects_newline {
                f32::from(end.x)
            } else {
                f32::from(bounds.size.width)
            };
            if right <= left {
                continue;
            }
            let top = bounds.top()
                + line.y
                + layout.line_height * row as f32
                + px(vertical_inset - scroll_y);
            quads.push(fill(
                Bounds::new(
                    point(bounds.left() + px(left - scroll_x), top),
                    size(px(right - left), layout.line_height),
                ),
                theme::accent().opacity(0.28),
            ));
        }
    }
    quads
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoundaryClass {
    Whitespace,
    Word,
    Punctuation,
}

fn boundary_class(grapheme: &str) -> BoundaryClass {
    if grapheme.chars().all(char::is_whitespace) {
        BoundaryClass::Whitespace
    } else if grapheme
        .chars()
        .any(|character| character.is_alphanumeric() || character == '_')
    {
        BoundaryClass::Word
    } else {
        BoundaryClass::Punctuation
    }
}

fn previous_grapheme_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .rev()
        .find_map(|(index, _)| (index < offset).then_some(index))
        .unwrap_or(0)
}

fn next_grapheme_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .find_map(|(index, _)| (index > offset).then_some(index))
        .unwrap_or(text.len())
}

fn previous_word_boundary(text: &str, offset: usize) -> usize {
    let graphemes: Vec<_> = text
        .grapheme_indices(true)
        .take_while(|(index, _)| *index < offset)
        .collect();
    let mut index = graphemes.len();
    while index > 0 && boundary_class(graphemes[index - 1].1) == BoundaryClass::Whitespace {
        index -= 1;
    }
    if index == 0 {
        return 0;
    }
    let class = boundary_class(graphemes[index - 1].1);
    while index > 0 && boundary_class(graphemes[index - 1].1) == class {
        index -= 1;
    }
    graphemes.get(index).map_or(0, |(offset, _)| *offset)
}

fn next_word_boundary(text: &str, offset: usize) -> usize {
    let graphemes: Vec<_> = text
        .grapheme_indices(true)
        .filter(|(index, _)| *index >= offset)
        .collect();
    let mut index = 0;
    while index < graphemes.len() && boundary_class(graphemes[index].1) == BoundaryClass::Whitespace
    {
        index += 1;
    }
    if index == graphemes.len() {
        return text.len();
    }
    let class = boundary_class(graphemes[index].1);
    while index < graphemes.len() && boundary_class(graphemes[index].1) == class {
        index += 1;
    }
    graphemes
        .get(index)
        .map_or(text.len(), |(offset, _)| *offset)
}

fn word_range_at(text: &str, offset: usize) -> Range<usize> {
    if text.is_empty() {
        return 0..0;
    }
    let offset = offset.min(text.len());
    for (start, segment) in text.split_word_bound_indices() {
        let end = start + segment.len();
        if (start..end).contains(&offset) || (offset == text.len() && end == text.len()) {
            return start..end;
        }
    }
    offset..offset
}

fn line_range_at(text: &str, offset: usize) -> Range<usize> {
    let offset = offset.min(text.len());
    let start = text[..offset]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |position| offset + position + 1);
    start..end
}

fn logical_line_edge(text: &str, offset: usize, end: bool) -> usize {
    let offset = offset.min(text.len());
    if end {
        text[offset..]
            .find('\n')
            .map_or(text.len(), |position| offset + position)
    } else {
        text[..offset]
            .rfind('\n')
            .map_or(0, |position| position + 1)
    }
}

fn normalize_inserted_text(text: &str, multiline: bool) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if multiline {
        normalized
    } else {
        normalized.replace('\n', " ")
    }
}

fn offset_from_utf16_in(text: &str, offset: usize) -> usize {
    let mut utf8 = 0;
    let mut utf16 = 0;
    for character in text.chars() {
        if utf16 >= offset {
            break;
        }
        utf16 += character.len_utf16();
        utf8 += character.len_utf8();
    }
    utf8
}

fn offset_to_utf16_in(text: &str, offset: usize) -> usize {
    let mut utf8 = 0;
    let mut utf16 = 0;
    for character in text.chars() {
        if utf8 >= offset {
            break;
        }
        utf8 += character.len_utf8();
        utf16 += character.len_utf16();
    }
    utf16
}

#[cfg(test)]
mod tests {
    use super::{
        line_range_at, logical_line_edge, next_grapheme_boundary, next_word_boundary,
        normalize_inserted_text, offset_from_utf16_in, offset_to_utf16_in,
        previous_grapheme_boundary, previous_word_boundary, word_range_at,
    };

    #[test]
    fn navigation_respects_graphemes_words_and_lines() {
        let text = "Hello, tall 👩🏽‍🎨 world\nsecond line";
        let emoji = text.find('👩').expect("emoji");
        assert_eq!(
            next_grapheme_boundary(text, emoji),
            text.find(" world").unwrap()
        );
        assert_eq!(
            previous_grapheme_boundary(text, text.find(" world").unwrap()),
            emoji
        );
        assert_eq!(
            previous_word_boundary(text, text.find("world").unwrap()),
            emoji
        );
        assert_eq!(
            next_word_boundary(text, text.find("world").unwrap()),
            text.find('\n').unwrap()
        );
        assert_eq!(word_range_at(text, 1), 0..5);
        assert_eq!(line_range_at(text, 2), 0..text.find('\n').unwrap() + 1);
        assert_eq!(
            logical_line_edge(text, text.len(), false),
            text.find('\n').unwrap() + 1
        );
    }

    #[test]
    fn single_line_paste_is_sanitized_without_breaking_multiline_text() {
        assert_eq!(
            normalize_inserted_text("one\r\ntwo\rthree", false),
            "one two three"
        );
        assert_eq!(
            normalize_inserted_text("one\r\ntwo\rthree", true),
            "one\ntwo\nthree"
        );
    }

    #[test]
    fn utf16_conversion_handles_non_bmp_input() {
        let text = "a👩🏽‍🎨b";
        for offset in text
            .char_indices()
            .map(|(offset, _)| offset)
            .chain([text.len()])
        {
            let utf16 = offset_to_utf16_in(text, offset);
            assert_eq!(offset_from_utf16_in(text, utf16), offset);
        }
    }
}
