//! The editing commands bound to keys: cursor movement, selection, deletion,
//! clipboard, and undo history.

use super::input::{
    Backspace, Copy, Cut, Delete, DeleteToLineEnd, DeleteToLineStart, DeleteWordBackward,
    DeleteWordForward, DocumentEnd, DocumentStart, Down, EditKind, End, Home, InsertNewline, Left,
    Paste, Redo, Right, SelectAll, SelectDocumentEnd, SelectDocumentStart, SelectDown, SelectEnd,
    SelectHome, SelectLeft, SelectRight, SelectUp, SelectWordLeft, SelectWordRight,
    ShowCharacterPalette, TextInput, TextInputEvent, Undo, Up, WordLeft, WordRight,
};
use super::input_text::{
    logical_line_edge, next_grapheme_boundary, next_word_boundary, previous_grapheme_boundary,
    previous_word_boundary,
};
use gpui::{ClipboardEntry, ClipboardItem, Context, Window, point, px};

impl TextInput {
    pub(super) fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(previous_grapheme_boundary(&self.content, self.cursor()), cx);
        } else {
            self.move_to(self.selected.start, cx);
        }
    }

    pub(super) fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(next_grapheme_boundary(&self.content, self.cursor()), cx);
        } else {
            self.move_to(self.selected.end, cx);
        }
    }

    pub(super) fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(previous_word_boundary(&self.content, self.cursor()), cx);
        } else {
            self.move_to(self.selected.start, cx);
        }
    }

    pub(super) fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(next_word_boundary(&self.content, self.cursor()), cx);
        } else {
            self.move_to(self.selected.end, cx);
        }
    }

    pub(super) fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(previous_grapheme_boundary(&self.content, self.cursor()), cx);
    }

    pub(super) fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(next_grapheme_boundary(&self.content, self.cursor()), cx);
    }

    pub(super) fn select_word_left(
        &mut self,
        _: &SelectWordLeft,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(previous_word_boundary(&self.content, self.cursor()), cx);
    }

    pub(super) fn select_word_right(
        &mut self,
        _: &SelectWordRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((target, preferred_x)) = self.vertical_target(-1) {
            self.move_to_with_preference(target, Some(preferred_x), cx);
        } else {
            self.move_to(0, cx);
        }
    }

    pub(super) fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((target, preferred_x)) = self.vertical_target(1) {
            self.move_to_with_preference(target, Some(preferred_x), cx);
        } else {
            self.move_to(self.content.len(), cx);
        }
    }

    pub(super) fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((target, preferred_x)) = self.vertical_target(-1) {
            self.select_to_vertical(target, preferred_x, cx);
        } else {
            self.select_to(0, cx);
        }
    }

    pub(super) fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.visual_line_edge(false), cx);
    }

    pub(super) fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.visual_line_edge(true), cx);
    }

    pub(super) fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.visual_line_edge(false), cx);
    }

    pub(super) fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.visual_line_edge(true), cx);
    }

    pub(super) fn document_start(
        &mut self,
        _: &DocumentStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_to(0, cx);
    }

    pub(super) fn document_end(&mut self, _: &DocumentEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    pub(super) fn select_document_start(
        &mut self,
        _: &SelectDocumentStart,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(0, cx);
    }

    pub(super) fn select_document_end(
        &mut self,
        _: &SelectDocumentEnd,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.content.len(), cx);
    }

    pub(super) fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = 0..self.content.len();
        self.selection_reversed = false;
        self.preferred_x = None;
        self.break_history_group();
        cx.notify();
    }

    /// Deletes the selection, or the range `to_boundary` reaches from the
    /// cursor when nothing is selected. Rings the bell when the cursor already
    /// sits at the edge it would delete towards.
    fn delete_to(
        &mut self,
        to_boundary: impl FnOnce(&Self) -> usize,
        kind: EditKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = if self.selected.is_empty() {
            let boundary = to_boundary(self);
            let cursor = self.cursor();
            boundary.min(cursor)..boundary.max(cursor)
        } else {
            self.selected.clone()
        };
        if range.is_empty() {
            window.play_system_bell();
        } else {
            self.apply_edit(range, "", kind, cx);
        }
    }

    pub(super) fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_to(
            |input| previous_grapheme_boundary(&input.content, input.cursor()),
            EditKind::DeleteBackward,
            window,
            cx,
        );
    }

    pub(super) fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_to(
            |input| next_grapheme_boundary(&input.content, input.cursor()),
            EditKind::DeleteForward,
            window,
            cx,
        );
    }

    pub(super) fn delete_word_backward(
        &mut self,
        _: &DeleteWordBackward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_to(
            |input| previous_word_boundary(&input.content, input.cursor()),
            EditKind::DeleteBackward,
            window,
            cx,
        );
    }

    pub(super) fn delete_word_forward(
        &mut self,
        _: &DeleteWordForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_to(
            |input| next_word_boundary(&input.content, input.cursor()),
            EditKind::DeleteForward,
            window,
            cx,
        );
    }

    pub(super) fn delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_to(
            |input| input.visual_line_edge(false),
            EditKind::DeleteBackward,
            window,
            cx,
        );
    }

    pub(super) fn delete_to_line_end(
        &mut self,
        _: &DeleteToLineEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_to(
            |input| input.visual_line_edge(true),
            EditKind::DeleteForward,
            window,
            cx,
        );
    }

    pub(super) fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let text = item.text();
        let was_empty = item.entries().is_empty();
        let mut images = Vec::new();
        let mut paths = Vec::new();
        for entry in item.into_entries() {
            match entry {
                ClipboardEntry::Image(image) => images.push(image),
                ClipboardEntry::ExternalPaths(external) => {
                    paths.extend(external.paths().iter().cloned());
                }
                ClipboardEntry::String(_) => {}
            }
        }
        if !images.is_empty() {
            cx.emit(TextInputEvent::PastedImages(images.into()));
        }
        if !paths.is_empty() {
            cx.emit(TextInputEvent::PastedPaths(paths));
        }
        if let Some(text) = text {
            let range = self.selected.clone();
            self.apply_edit(range, &text, EditKind::Replace, cx);
        } else if was_empty {
            window.play_system_bell();
        }
    }

    pub(super) fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected.clone()].to_owned(),
            ));
        }
    }

    pub(super) fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            window.play_system_bell();
            return;
        }
        self.copy(&Copy, window, cx);
        self.apply_edit(self.selected.clone(), "", EditKind::Replace, cx);
    }

    pub(super) fn undo(&mut self, _: &Undo, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn redo(&mut self, _: &Redo, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn insert_newline(
        &mut self,
        _: &InsertNewline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode.is_multiline() {
            self.apply_edit(self.selected.clone(), "\n", EditKind::Replace, cx);
        } else {
            window.play_system_bell();
        }
    }

    pub(super) fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }
}
