//! The custom element that shapes, scrolls, and paints the editor's text.

use super::input::{LINE_HEIGHT, TextInput, TextInputMode};
use super::input_layout::{TextLayout, selection_quads};
use super::theme;
use gpui::{
    App, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity, GlobalElementId,
    LayoutId, PaintQuad, Pixels, Point, Style, TextRun, UnderlineStyle, Window, fill, point,
    prelude::*, px, relative, size,
};
use std::sync::Arc;

pub(super) struct TextElement {
    pub(super) input: Entity<TextInput>,
}

pub(super) struct Prepaint {
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
