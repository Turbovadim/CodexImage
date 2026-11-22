//! Geometry for shaped text: mapping between byte offsets and pixel positions,
//! and the quads that highlight a selection.

use gpui::{Bounds, Hsla, PaintQuad, Pixels, Point, WrappedLine, fill, point, px, size};
use std::ops::Range;
use std::sync::Arc;

/// A text offset, plus the row it sits on when it falls on a soft wrap: there
/// the end of one row and the start of the next share an offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Caret {
    pub(super) offset: usize,
    pub(super) row_end: bool,
}

impl Caret {
    pub(super) fn at(offset: usize) -> Self {
        Self {
            offset,
            row_end: false,
        }
    }
}

pub(super) struct LayoutLine {
    pub(super) shaped: Arc<WrappedLine>,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) y: Pixels,
    pub(super) height: Pixels,
    pub(super) has_newline: bool,
    /// Offsets within the line where each soft-wrapped row after the first begins.
    row_starts: Vec<usize>,
}

impl LayoutLine {
    fn position(&self, local: usize, row_end: bool, line_height: Pixels) -> Point<Pixels> {
        if !row_end && let Some(row) = self.row_starts.iter().position(|start| *start == local) {
            return point(px(0.), line_height * (row + 1) as f32);
        }
        self.shaped
            .position_for_index(local, line_height)
            .unwrap_or_default()
    }
}

pub(super) struct TextLayout {
    pub(super) lines: Vec<LayoutLine>,
    pub(super) content_len: usize,
    pub(super) line_height: Pixels,
    pub(super) total_height: Pixels,
    pub(super) max_width: Pixels,
    pub(super) visual_line_count: usize,
}

impl TextLayout {
    pub(super) fn new(shaped_lines: Vec<WrappedLine>, content: &str, line_height: Pixels) -> Self {
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
            let row_starts: Vec<usize> = shaped
                .wrap_boundaries()
                .iter()
                .map(|boundary| shaped.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index)
                .collect();
            let shaped = Arc::new(shaped);
            let height = shaped.size(line_height).height;
            max_width = max_width.max(shaped.width());
            visual_line_count += row_starts.len() + 1;
            lines.push(LayoutLine {
                shaped,
                start,
                end,
                y,
                height,
                has_newline,
                row_starts,
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

    pub(super) fn position_for(&self, caret: Caret) -> Point<Pixels> {
        let offset = caret.offset.min(self.content_len);
        let Some(line) = self
            .lines
            .iter()
            .find(|line| offset <= line.end)
            .or(self.lines.last())
        else {
            return Point::default();
        };
        let local = offset.saturating_sub(line.start).min(line.end - line.start);
        let position = line.position(local, caret.row_end, self.line_height);
        point(position.x, line.y + position.y)
    }

    /// The caret nearest `position`. Anything above the text resolves to its
    /// start and anything below to its end.
    pub(super) fn caret_for_position(&self, position: Point<Pixels>) -> Caret {
        if self.content_len == 0 || self.lines.is_empty() || position.y < px(0.) {
            return Caret::at(0);
        }
        let Some(line) = self
            .lines
            .iter()
            .find(|line| position.y < line.y + line.height)
        else {
            return Caret::at(self.content_len);
        };
        let local_position = point(position.x, position.y - line.y);
        let local = line
            .shaped
            .closest_index_for_position(local_position, self.line_height)
            .unwrap_or_else(|index| index)
            .min(line.end - line.start);
        let row = (local_position.y / self.line_height) as usize;
        Caret {
            offset: line.start + local,
            row_end: line.row_starts.get(row) == Some(&local),
        }
    }

    pub(super) fn visual_line_edge(&self, caret: Caret, end: bool) -> Caret {
        let position = self.position_for(caret);
        self.caret_for_position(point(
            if end { px(1_000_000.) } else { px(0.) },
            position.y + self.line_height / 2.,
        ))
    }
}

pub(super) fn selection_quads(
    layout: &TextLayout,
    selected: &Range<usize>,
    bounds: Bounds<Pixels>,
    scroll_x: f32,
    scroll_y: f32,
    vertical_inset: f32,
    color: Hsla,
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
        let start = line.position(local_start, false, layout.line_height);
        let end = line.position(local_end, true, layout.line_height);
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
                color,
            ));
        }
    }
    quads
}
