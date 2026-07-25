//! The canvas surface: camera control, hit testing, node dragging, and the
//! elements layered over the painted board.

use super::app::AppView;
use super::app::Overlay;
use super::canvas::{
    CanvasNodeFrame, VIEWPORT_CULL_MARGIN, edge_is_visible, paint_canvas_node, paint_connectors,
    paint_dot_grid, rect_is_visible,
};
use super::card::{
    ATTACHMENT_ROW_HEIGHT, COLLAPSED_PROMPT_LINES, CanvasNode, EXPANDED_PROMPT_LINES, MEDIA_GAP,
    OutputLayout, PROMPT_LINE_HEIGHT, SHOW_MORE_HEIGHT, status_area_height,
};
use super::keymap::{FitCanvas, ZoomIn, ZoomOut};
use super::theme;
use crate::layout::CARD_WIDTH;
use crate::layout::Position;
use crate::model::{BoardNode, NodeStatus};
use crate::storage::now_ms;
use gpui::{
    AnyElement, Bounds, ClipboardItem, Context, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PinchEvent, Pixels, Point, ScrollWheelEvent, SharedString, Window, canvas, div,
    fill, point, prelude::*, px, size,
};

pub(super) const NODE_TOOLBAR_HEIGHT: f32 = 36.;

#[derive(Clone)]
pub(super) enum CanvasClickTarget {
    Image { node_id: String, url: String },
    TogglePrompt(String),
    Retry(String),
}

pub(super) enum DragState {
    Canvas {
        start: Point<Pixels>,
        origin: (f32, f32),
    },
    Lightbox {
        start: Point<Pixels>,
        origin: (f32, f32),
    },
    Node {
        id: String,
        start: Point<Pixels>,
        origin: Position,
        click_target: Option<CanvasClickTarget>,
    },
}

impl AppView {
    fn fit_canvas(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(board) = &self.board else { return };
        if board.nodes.is_empty() {
            return;
        }
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for node in &board.nodes {
            let Some(position) = self.current_position(&node.id) else {
                continue;
            };
            min_x = min_x.min(position.x);
            min_y = min_y.min(position.y);
            max_x = max_x.max(position.x + CARD_WIDTH);
            max_y = max_y.max(position.y + self.card_height(node));
        }
        let viewport = window.viewport_size();
        let width = f32::from(viewport.width) - 100.;
        let height = f32::from(viewport.height) - 150.;
        self.zoom = (width / (max_x - min_x).max(1.))
            .min(height / (max_y - min_y).max(1.))
            .clamp(0.08, 1.);
        self.camera_x =
            (f32::from(viewport.width) - (max_x - min_x) * self.zoom) / 2. - min_x * self.zoom;
        self.camera_y =
            (f32::from(viewport.height) - (max_y - min_y) * self.zoom) / 2. - min_y * self.zoom;
        cx.notify();
    }

    pub(super) fn fit_action(
        &mut self,
        _: &FitCanvas,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.overlay, Overlay::None) {
            self.fit_canvas(window, cx)
        }
    }

    pub(super) fn zoom_in(&mut self, _: &ZoomIn, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size();
        self.zoom_at(point(viewport.width / 2., viewport.height / 2.), 1.25, cx);
    }

    pub(super) fn zoom_out(&mut self, _: &ZoomOut, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size();
        self.zoom_at(point(viewport.width / 2., viewport.height / 2.), 0.8, cx);
    }

    fn zoom_at(&mut self, position: Point<Pixels>, factor: f32, cx: &mut Context<Self>) {
        let old = self.zoom;
        let new = (old * factor).clamp(0.08, 2.);
        let world_x = (f32::from(position.x) - self.camera_x) / old;
        let world_y = (f32::from(position.y) - self.camera_y) / old;
        self.camera_x = f32::from(position.x) - world_x * new;
        self.camera_y = f32::from(position.y) - world_y * new;
        self.zoom = new;
        cx.notify();
    }

    pub(super) fn locate_node(&mut self, node_id: &str, window: &Window, cx: &mut Context<Self>) {
        let Some(position) = self.current_position(node_id) else {
            return;
        };
        self.overlay = Overlay::None;
        self.zoom = 1.;
        self.camera_x =
            f32::from(window.viewport_size().width) / 2. - (position.x + CARD_WIDTH / 2.);
        self.camera_y = 100. - position.y;
        cx.notify();
    }

    pub(super) fn render_canvas(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let viewport = window.viewport_size();
        let minimap = self.render_minimap(viewport, cx);
        let viewport_width = f32::from(viewport.width);
        let viewport_height = f32::from(viewport.height);
        let board = self.board.as_ref();

        let visible_nodes = board
            .into_iter()
            .flat_map(|board| board.nodes.iter().enumerate())
            .filter_map(|(node_index, node)| {
                let position = self.current_position(&node.id)?;
                let screen_x = self.camera_x + position.x * self.zoom;
                let screen_y = self.camera_y + position.y * self.zoom;
                let height = self.card_height(node) * self.zoom;
                rect_is_visible(
                    screen_x,
                    screen_y,
                    CARD_WIDTH * self.zoom,
                    height,
                    viewport_width,
                    viewport_height,
                    VIEWPORT_CULL_MARGIN,
                )
                .then_some(CanvasNodeFrame {
                    node_index,
                    screen_x,
                    screen_y,
                    height,
                    targeted: self
                        .target
                        .as_ref()
                        .is_some_and(|target| target.node_id == node.id),
                })
            })
            .collect::<Vec<_>>();

        let hovered_toolbar = self.hovered_node.as_deref().and_then(|hovered_id| {
            visible_nodes.iter().find_map(|frame| {
                let node = self.canvas_nodes.get(frame.node_index)?;
                (node.node.id == hovered_id)
                    .then(|| self.render_node_toolbar(&node.node, *frame, cx))
            })
        });
        let edge_points: Vec<_> = board
            .into_iter()
            .flat_map(|board| &board.nodes)
            .filter_map(|node| {
                let parent = node.parent_id.as_deref()?;
                let parent_position = self.current_position(parent)?;
                let node_position = self.current_position(&node.id)?;
                let parent_height = self.heights.get(parent).copied()?;
                let from = point(
                    px(self.camera_x + (parent_position.x + CARD_WIDTH / 2.) * self.zoom),
                    px(self.camera_y + (parent_position.y + parent_height) * self.zoom),
                );
                let to = point(
                    px(self.camera_x + (node_position.x + CARD_WIDTH / 2.) * self.zoom),
                    px(self.camera_y + node_position.y * self.zoom),
                );
                edge_is_visible(
                    from,
                    to,
                    viewport_width,
                    viewport_height,
                    VIEWPORT_CULL_MARGIN,
                )
                .then_some((from, to))
            })
            .collect();
        let canvas_nodes = self.canvas_nodes.clone();
        let activity = self.activity.clone();
        let zoom = self.zoom;
        let camera_x = self.camera_x;
        let camera_y = self.camera_y;
        let now = now_ms();
        let background = canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                paint_dot_grid(bounds, camera_x, camera_y, zoom, window);
                paint_connectors(&edge_points, zoom, window);
                for frame in &visible_nodes {
                    if let Some(node) = canvas_nodes.get(frame.node_index) {
                        paint_canvas_node(*frame, node, zoom, &activity, now, window, cx);
                    }
                }
            },
        )
        .size_full();

        let mut layer = div()
            .id("canvas")
            .absolute()
            .inset_0()
            .overflow_hidden()
            .bg(theme::background())
            .cursor(gpui::CursorStyle::OpenHand)
            .on_scroll_wheel(
                cx.listener(|this, event: &ScrollWheelEvent, _, cx| this.scroll_canvas(event, cx)),
            )
            .on_pinch(cx.listener(|this, event: &PinchEvent, _, cx| this.pinch_canvas(event, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.canvas_mouse_down(event, window, cx);
                }),
            )
            .child(background);
        if let Some(toolbar) = hovered_toolbar {
            layer = layer.child(toolbar);
        }
        if let Some(minimap) = minimap {
            layer = layer.child(minimap);
        }
        layer.into_any_element()
    }

    fn render_node_toolbar(
        &self,
        node: &BoardNode,
        frame: CanvasNodeFrame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let scale = self.zoom;
        let id = node.id.clone();
        let prompt = node.prompt.clone();
        let running = node.status == NodeStatus::Running;
        let button = |key: &str, label: &'static str, color: gpui::Hsla| {
            div()
                .id(SharedString::from(format!("{key}-{id}")))
                .px(px(8. * scale))
                .py(px(4. * scale))
                .rounded(px(6. * scale))
                .bg(theme::background().opacity(0.9))
                .text_color(color)
                .text_size(px(12. * scale))
                .child(label)
        };
        let node_id = node.id.clone();
        let toolbar = div()
            .absolute()
            .top(px(-36. * scale))
            .right(px(4. * scale))
            .flex()
            .gap(px(4. * scale))
            .occlude()
            .children(running.then(|| {
                let node_id = node_id.clone();
                button("stop", "Stop", theme::danger())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.engine.stop_node(&node_id);
                    }))
                    .into_any_element()
            }))
            .children((!running).then(|| {
                let node_id = node_id.clone();
                button("branch", "Branch", theme::ink())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.branch_node(&node_id, None, window, cx);
                    }))
                    .into_any_element()
            }))
            .children((!running).then(|| {
                let node_id = node_id.clone();
                button("edit", "Edit", theme::dim())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.edit_node(&node_id, window, cx);
                    }))
                    .into_any_element()
            }))
            .children((!running).then(|| {
                let node_id = node_id.clone();
                button("regen", "Retry", theme::dim())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.regenerate_node(&node_id, cx);
                    }))
                    .into_any_element()
            }))
            .child(
                button("copy-prompt", "Copy", theme::dim()).on_click(cx.listener(
                    move |this, _, _, cx| {
                        cx.stop_propagation();
                        cx.write_to_clipboard(ClipboardItem::new_string(prompt.clone()));
                        this.show_toast("Prompt copied".into(), false, None, cx);
                    },
                )),
            )
            .child(button("dup", "Dup", theme::dim()).on_click({
                let node_id = node_id.clone();
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.duplicate_node(&node_id, cx);
                })
            }))
            .child(
                button("del", "Delete", theme::danger()).on_click(cx.listener(
                    move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.delete_node(&node_id, cx);
                    },
                )),
            );
        div()
            .absolute()
            .left(px(frame.screen_x))
            .top(px(frame.screen_y))
            .w(px(CARD_WIDTH * scale))
            .h(px(frame.height))
            .child(toolbar)
            .into_any_element()
    }

    fn render_minimap(
        &self,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        const WIDTH: f32 = 142.;
        const HEIGHT: f32 = 96.;
        const PADDING: f32 = 6.;
        const RIGHT: f32 = 18.;
        const BOTTOM: f32 = 24.;

        let board = self.board.as_ref()?;
        if board.nodes.len() < 2 {
            return None;
        }
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for node in &board.nodes {
            let position = self.current_position(&node.id)?;
            min_x = min_x.min(position.x);
            min_y = min_y.min(position.y);
            max_x = max_x.max(position.x + CARD_WIDTH);
            max_y = max_y.max(position.y + self.card_height(node));
        }
        let world_width = (max_x - min_x).max(1.);
        let world_height = (max_y - min_y).max(1.);
        let scale =
            ((WIDTH - PADDING * 2.) / world_width).min((HEIGHT - PADDING * 2.) / world_height);
        let offset_x = PADDING + (WIDTH - PADDING * 2. - world_width * scale) / 2.;
        let offset_y = PADDING + (HEIGHT - PADDING * 2. - world_height * scale) / 2.;
        let node_rects: Vec<_> = board
            .nodes
            .iter()
            .filter_map(|node| {
                let position = self.current_position(&node.id)?;
                Some((
                    offset_x + (position.x - min_x) * scale,
                    offset_y + (position.y - min_y) * scale,
                    (CARD_WIDTH * scale).max(2.),
                    (self.card_height(node) * scale).max(2.),
                    node.status,
                ))
            })
            .collect();
        let viewport_width = f32::from(viewport.width);
        let viewport_height = f32::from(viewport.height);
        let visible_world_x = -self.camera_x / self.zoom;
        let visible_world_y = -self.camera_y / self.zoom;
        let visible_rect = (
            offset_x + (visible_world_x - min_x) * scale,
            offset_y + (visible_world_y - min_y) * scale,
            viewport_width / self.zoom * scale,
            viewport_height / self.zoom * scale,
        );
        let map = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                for (x, y, width, height, status) in node_rects {
                    let color = match status {
                        NodeStatus::Running => theme::accent(),
                        NodeStatus::Error => theme::danger(),
                        _ => theme::dim(),
                    };
                    window.paint_quad(fill(
                        Bounds::new(
                            point(bounds.left() + px(x), bounds.top() + px(y)),
                            size(px(width), px(height)),
                        ),
                        color.opacity(0.78),
                    ));
                }
                window.paint_quad(fill(
                    Bounds::new(
                        point(
                            bounds.left() + px(visible_rect.0),
                            bounds.top() + px(visible_rect.1),
                        ),
                        size(px(visible_rect.2), px(visible_rect.3)),
                    ),
                    theme::accent().opacity(0.18),
                ));
            },
        )
        .size_full();
        let origin_x = viewport_width - RIGHT - WIDTH;
        let origin_y = viewport_height - BOTTOM - HEIGHT;
        Some(
            div()
                .id("minimap")
                .absolute()
                .right(px(RIGHT))
                .bottom(px(BOTTOM))
                .w(px(WIDTH))
                .h(px(HEIGHT))
                .overflow_hidden()
                .rounded_lg()
                .border_1()
                .border_color(theme::line())
                .bg(theme::raised().opacity(0.94))
                .cursor_pointer()
                .occlude()
                .child(map)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let local_x = f32::from(event.position.x) - origin_x;
                        let local_y = f32::from(event.position.y) - origin_y;
                        let world_x = min_x + (local_x - offset_x) / scale;
                        let world_y = min_y + (local_y - offset_y) / scale;
                        this.camera_x = viewport_width / 2. - world_x * this.zoom;
                        this.camera_y = viewport_height / 2. - world_y * this.zoom;
                        cx.notify();
                    }),
                )
                .into_any_element(),
        )
    }

    fn canvas_node_at(&self, position: Point<Pixels>) -> Option<(usize, Position)> {
        let x = f32::from(position.x);
        let y = f32::from(position.y);
        self.canvas_nodes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, canvas_node)| {
                let world_position = self.current_position(&canvas_node.node.id)?;
                let screen_x = self.camera_x + world_position.x * self.zoom;
                let screen_y = self.camera_y + world_position.y * self.zoom;
                let width = CARD_WIDTH * self.zoom;
                let height = self.card_height(&canvas_node.node) * self.zoom;
                let toolbar_top = screen_y - NODE_TOOLBAR_HEIGHT * self.zoom;
                (x >= screen_x
                    && x <= screen_x + width
                    && y >= toolbar_top
                    && y <= screen_y + height)
                    .then_some((index, world_position))
            })
    }

    fn canvas_click_target(
        &self,
        canvas_node: &CanvasNode,
        position: Point<Pixels>,
        world_position: Position,
    ) -> Option<CanvasClickTarget> {
        let local_x = (f32::from(position.x) - self.camera_x) / self.zoom - world_position.x;
        let local_y = (f32::from(position.y) - self.camera_y) / self.zoom - world_position.y;
        let expanded = self.expanded_prompts.contains(&canvas_node.node.id);
        let prompt_clamped = canvas_node.prompt_lines.len() > COLLAPSED_PROMPT_LINES;
        let visible_lines = if expanded {
            canvas_node.prompt_lines.len().min(EXPANDED_PROMPT_LINES)
        } else {
            canvas_node
                .collapsed_prompt_lines
                .len()
                .min(COLLAPSED_PROMPT_LINES)
        }
        .max(1);
        let prompt_height = 24.
            + visible_lines as f32 * PROMPT_LINE_HEIGHT
            + if prompt_clamped { SHOW_MORE_HEIGHT } else { 0. };
        if prompt_clamped
            && local_y >= 13. + visible_lines as f32 * PROMPT_LINE_HEIGHT
            && local_y <= prompt_height
        {
            return Some(CanvasClickTarget::TogglePrompt(canvas_node.node.id.clone()));
        }

        let attachment_height = if canvas_node.attachment_images.is_empty() {
            0.
        } else {
            ATTACHMENT_ROW_HEIGHT
        };
        let media_top = prompt_height + attachment_height + 26. + MEDIA_GAP;
        let media_y = local_y - media_top;
        if media_y >= 0. && media_y <= canvas_node.output_layout.height() {
            let image_index = match &canvas_node.output_layout {
                OutputLayout::None => None,
                OutputLayout::Tiles { cells, .. } => cells.iter().find_map(|cell| {
                    (local_x >= cell.x
                        && local_x <= cell.x + cell.width
                        && media_y >= cell.y
                        && media_y <= cell.y + cell.height)
                        .then_some(cell.index)
                }),
                OutputLayout::Filmstrip {
                    hero_height,
                    compact_count,
                    strip_cell_width,
                    ..
                } => {
                    if media_y <= *hero_height {
                        Some(0)
                    } else {
                        let strip_y = media_y - *hero_height - MEDIA_GAP;
                        if strip_y < 0. || strip_y > *strip_cell_width {
                            None
                        } else {
                            (0..*compact_count).find_map(|compact_index| {
                                let x = compact_index as f32 * (*strip_cell_width + MEDIA_GAP);
                                (local_x >= x && local_x <= x + *strip_cell_width)
                                    .then_some(compact_index + 1)
                            })
                        }
                    }
                }
            };
            if let Some(image) =
                image_index.and_then(|index| canvas_node.displayed_images.get(index))
            {
                return Some(CanvasClickTarget::Image {
                    node_id: canvas_node.node.id.clone(),
                    url: image.url.clone(),
                });
            }
        }

        let status_y = media_top + canvas_node.output_layout.height();
        let in_retry = match canvas_node.node.status {
            NodeStatus::Error if canvas_node.displayed_images.is_empty() => {
                (CARD_WIDTH / 2. - 29. ..=CARD_WIDTH / 2. + 29.).contains(&local_x)
                    && (status_y + 88. ..=status_y + 114.).contains(&local_y)
            }
            NodeStatus::Error | NodeStatus::Stopped => {
                (CARD_WIDTH - 68. ..=CARD_WIDTH - 14.).contains(&local_x)
                    && (status_y..=status_y + status_area_height(&canvas_node.node))
                        .contains(&local_y)
            }
            NodeStatus::Running | NodeStatus::Done => false,
        };
        in_retry.then(|| CanvasClickTarget::Retry(canvas_node.node.id.clone()))
    }

    fn canvas_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.overlay, Overlay::None) {
            return;
        }
        window.focus(&self.focus, cx);
        if let Some((index, origin)) = self.canvas_node_at(event.position) {
            let canvas_node = &self.canvas_nodes[index];
            self.drag = Some(DragState::Node {
                id: canvas_node.node.id.clone(),
                start: event.position,
                origin,
                click_target: self.canvas_click_target(canvas_node, event.position, origin),
            });
        } else {
            self.drag = Some(DragState::Canvas {
                start: event.position,
                origin: (self.camera_x, self.camera_y),
            });
        }
        cx.notify();
    }

    fn scroll_canvas(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        if !matches!(self.overlay, Overlay::None) {
            return;
        }
        let delta = event.delta.pixel_delta(px(18.));
        if event.modifiers.control || event.modifiers.platform {
            let factor = (-f32::from(delta.y) * 0.004).exp();
            self.zoom_at(event.position, factor, cx);
        } else {
            self.camera_x += f32::from(delta.x);
            self.camera_y += f32::from(delta.y);
            cx.notify();
        }
    }

    fn pinch_canvas(&mut self, event: &PinchEvent, cx: &mut Context<Self>) {
        if matches!(self.overlay, Overlay::None) {
            self.zoom_at(event.position, 1. + event.delta, cx)
        }
    }

    pub(super) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.overlay, Overlay::None) {
            return;
        }
        match &self.drag {
            Some(DragState::Canvas { start, origin }) if event.dragging() => {
                self.camera_x = origin.0 + f32::from(event.position.x - start.x);
                self.camera_y = origin.1 + f32::from(event.position.y - start.y);
                cx.notify();
            }
            Some(DragState::Node {
                id, start, origin, ..
            }) if event.dragging()
                && (f32::from(event.position.x - start.x).powi(2)
                    + f32::from(event.position.y - start.y).powi(2))
                    >= 9. =>
            {
                self.transient_positions.insert(
                    id.clone(),
                    Position {
                        x: origin.x + f32::from(event.position.x - start.x) / self.zoom,
                        y: origin.y + f32::from(event.position.y - start.y) / self.zoom,
                    },
                );
                cx.notify();
            }
            _ if !event.dragging() => {
                let hovered = self
                    .canvas_node_at(event.position)
                    .and_then(|(index, _)| self.canvas_nodes.get(index))
                    .map(|node| node.node.id.clone());
                if hovered != self.hovered_node {
                    self.hovered_node = hovered;
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    pub(super) fn mouse_up(
        &mut self,
        _: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.overlay, Overlay::None) {
            self.drag = None;
            return;
        }
        let drag = self.drag.take();
        if let Some(DragState::Node {
            id, click_target, ..
        }) = drag
        {
            if let Some(position) = self.transient_positions.remove(&id) {
                self.on_board(cx, |this, board_id| {
                    this.engine
                        .repository()
                        .move_node(board_id, &id, position.x, position.y)
                        .map(|_| ())
                });
            } else if let Some(click_target) = click_target {
                match click_target {
                    CanvasClickTarget::Image { node_id, url } => {
                        self.open_lightbox(node_id, url, window, cx);
                    }
                    CanvasClickTarget::TogglePrompt(node_id) => {
                        if !self.expanded_prompts.remove(&node_id) {
                            self.expanded_prompts.insert(node_id);
                        }
                        self.refresh_layout();
                    }
                    CanvasClickTarget::Retry(node_id) => self.regenerate_node(&node_id, cx),
                }
            }
        }
        cx.notify();
    }
}
