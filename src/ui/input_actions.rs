//! The editing commands bound to keys: cursor movement, selection, deletion,
//! clipboard, and undo history.

use super::input::{
    Copy, Cut, Delete, EditKind, InsertNewline, Motion, Move, Paste, Redo, Select, SelectAll,
    ShowCharacterPalette, TextInput, TextInputEvent, Undo,
};
use super::input_layout::Caret;
use super::input_text::{
    logical_line_edge, next_grapheme_boundary, next_word_boundary, previous_grapheme_boundary,
    previous_word_boundary,
};
use gpui::{ClipboardEntry, ClipboardItem, Context, Window, point, px};

impl TextInput {
    /// Where `motion` takes the cursor, and for vertical motions the column
    /// to keep aiming for, so repeated up/down holds its place across short
    /// lines.
    fn motion_target(&self, motion: Motion) -> (Caret, Option<f32>) {
        let text = &self.content;
        let cursor = self.cursor();
        let layout = self.last_layout.as_ref();
        let offset = match motion {
            Motion::Left => previous_grapheme_boundary(text, cursor),
            Motion::Right => next_grapheme_boundary(text, cursor),
            Motion::WordLeft => previous_word_boundary(text, cursor),
            Motion::WordRight => next_word_boundary(text, cursor),
            Motion::DocumentStart => 0,
            Motion::DocumentEnd => text.len(),
            Motion::Up | Motion::Down => {
                let up = motion == Motion::Up;
                let Some(layout) = layout else {
                    return (Caret::at(if up { 0 } else { text.len() }), None);
                };
                let position = layout.position_for(self.caret());
                let preferred_x = self.preferred_x.unwrap_or_else(|| f32::from(position.x));
                let rows = if up { -0.5 } else { 1.5 };
                let target = point(px(preferred_x), position.y + layout.line_height * rows);
                return (layout.caret_for_position(target), Some(preferred_x));
            }
            Motion::LineStart | Motion::LineEnd => {
                let end = motion == Motion::LineEnd;
                let edge = layout.map_or_else(
                    || Caret::at(logical_line_edge(text, cursor, end)),
                    |layout| layout.visual_line_edge(self.caret(), end),
                );
                return (edge, None);
            }
        };
        (Caret::at(offset), None)
    }

    pub(super) fn move_cursor(
        &mut self,
        &Move(motion): &Move,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Stepping sideways out of a selection lands on its edge.
        let (target, preferred_x) = match motion {
            _ if self.selected.is_empty() => self.motion_target(motion),
            Motion::Left | Motion::WordLeft => (Caret::at(self.selected.start), None),
            Motion::Right | Motion::WordRight => (Caret::at(self.selected.end), None),
            _ => self.motion_target(motion),
        };
        self.move_to(target, cx);
        self.preferred_x = preferred_x;
    }

    pub(super) fn select(
        &mut self,
        &Select(motion): &Select,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (target, preferred_x) = self.motion_target(motion);
        self.select_to(target, cx);
        self.preferred_x = preferred_x;
    }

    /// Rings the bell when there is nothing in that direction to delete.
    pub(super) fn delete(
        &mut self,
        &Delete(motion): &Delete,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = if self.selected.is_empty() {
            let target = self.motion_target(motion).0.offset;
            let cursor = self.cursor();
            target.min(cursor)..target.max(cursor)
        } else {
            self.selected.clone()
        };
        if range.is_empty() {
            window.play_system_bell();
            return;
        }
        let kind = match motion {
            Motion::Left | Motion::WordLeft | Motion::LineStart => EditKind::DeleteBackward,
            _ => EditKind::DeleteForward,
        };
        self.replace(range, "", kind, cx);
    }

    pub(super) fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_content(cx);
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
            self.replace(self.selected.clone(), &text, EditKind::Replace, cx);
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
        self.replace(self.selected.clone(), "", EditKind::Replace, cx);
    }

    pub(super) fn undo(&mut self, _: &Undo, window: &mut Window, cx: &mut Context<Self>) {
        self.step_history(true, window, cx);
    }

    pub(super) fn redo(&mut self, _: &Redo, window: &mut Window, cx: &mut Context<Self>) {
        self.step_history(false, window, cx);
    }

    fn step_history(&mut self, undo: bool, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.snapshot();
        let (from, to) = if undo {
            (&mut self.undo_stack, &mut self.redo_stack)
        } else {
            (&mut self.redo_stack, &mut self.undo_stack)
        };
        let Some(snapshot) = from.pop() else {
            window.play_system_bell();
            return;
        };
        to.push(current);
        self.restore(snapshot);
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
            self.replace(self.selected.clone(), "\n", EditKind::Replace, cx);
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
