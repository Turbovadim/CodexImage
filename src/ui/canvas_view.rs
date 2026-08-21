//! The canvas surface: camera control, hit testing, node dragging, and the
//! elements layered over the painted board.

use super::app::AppView;
use super::app::Overlay;
use super::canvas::{
    CanvasNodeFrame, ToolbarButtonPaint, VIEWPORT_CULL_MARGIN, edge_is_visible, paint_canvas_node,
    paint_connectors, paint_dot_grid, paint_node_toolbar, rect_is_visible,
};
use super::card::{
    ATTACHMENT_ROW_HEIGHT, COLLAPSED_PROMPT_LINES, CanvasNode, CardRect, EXPANDED_PROMPT_LINES,
    MEDIA_GAP, OutputLayout, PROMPT_LINE_HEIGHT, SHOW_MORE_HEIGHT, attached_text_height,
    status_area_height,
};
use super::keymap::{FitCanvas, ResetZoom, ZoomIn, ZoomOut};
use super::theme;
use super::tooltip::{tip, tip_with_shortcut};
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
/// World-space toolbar metrics; everything scales with the canvas zoom so the
/// toolbar keeps a fixed size relative to its card.
const TOOLBAR_BUTTON_HEIGHT: f32 = 22.;
const TOOLBAR_GAP: f32 = 4.;
const TOOLBAR_CARD_GAP: f32 = 6.;
const TOOLBAR_RIGHT_MARGIN: f32 = 4.;
const MIN_ZOOM: f32 = 0.08;
const MAX_ZOOM: f32 = 2.;
/// How long zoom must hold still before sprites re-render for the new tier.
const ZOOM_SETTLE_DELAY: std::time::Duration = std::time::Duration::from_millis(150);
const MINIMAP_WIDTH: f32 = 142.;
const MINIMAP_HEIGHT: f32 = 96.;
const MINIMAP_RIGHT: f32 = 18.;
const MINIMAP_BOTTOM: f32 = 24.;
/// Vertical band reserved for the board switcher and gallery button.
const HEADER_CLEARANCE: f32 = 60.;

#[derive(Clone)]
pub(super) enum CanvasClickTarget {
    Image {
        node_id: String,
        url: String,
    },
    TogglePrompt(String),
    Retry(String),
    NodeText(String),
    Toolbar {
        node_id: String,
        action: ToolbarAction,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolbarAction {
    Stop,
    Branch,
    Edit,
    Retry,
    Copy,
    Duplicate,
    Delete,
}

impl ToolbarAction {
    fn label(self) -> &'static str {
        match self {
            Self::Stop => "Stop",
            Self::Branch => "Branch",
            Self::Edit => "Edit",
            Self::Retry => "Retry",
            Self::Copy => "Copy",
            Self::Duplicate => "Dup",
            Self::Delete => "Delete",
        }
    }

    fn color(self) -> gpui::Hsla {
        match self {
            Self::Stop | Self::Delete => theme::danger(),
            Self::Branch => theme::ink(),
            _ => theme::dim(),
        }
    }
}

/// The card-local world-space rectangles of the hovered card's action buttons,
/// right-aligned above the card (or below it near the viewport top).
fn toolbar_layout(running: bool, below: bool, card_height: f32) -> Vec<(ToolbarAction, CardRect)> {
    let actions: &[ToolbarAction] = if running {
        &[
            ToolbarAction::Stop,
            ToolbarAction::Copy,
            ToolbarAction::Duplicate,
            ToolbarAction::Delete,
        ]
    } else {
        &[
            ToolbarAction::Branch,
            ToolbarAction::Edit,
            ToolbarAction::Retry,
            ToolbarAction::Copy,
            ToolbarAction::Duplicate,
            ToolbarAction::Delete,
        ]
    };
    let width_of = |action: ToolbarAction| 14. + action.label().chars().count() as f32 * 6.6;
    let total = actions.iter().map(|action| width_of(*action)).sum::<f32>()
        + TOOLBAR_GAP * actions.len().saturating_sub(1) as f32;
    let y = if below {
        card_height + TOOLBAR_CARD_GAP
    } else {
        -(TOOLBAR_CARD_GAP + TOOLBAR_BUTTON_HEIGHT)
    };
    let mut x = CARD_WIDTH - TOOLBAR_RIGHT_MARGIN - total;
    actions
        .iter()
        .map(|action| {
            let width = width_of(*action);
            let rect = CardRect::new(x, y, width, TOOLBAR_BUTTON_HEIGHT);
            x += width + TOOLBAR_GAP;
            (*action, rect)
        })
        .collect()
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
    },
    NodeClick(CanvasClickTarget),
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
            .clamp(MIN_ZOOM, 1.);
        self.last_zoom_change = std::time::Instant::now();
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

    /// Returns the canvas to 1:1 without moving what is under the viewport centre.
    pub(super) fn reset_zoom(
        &mut self,
        _: &ResetZoom,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.overlay, Overlay::None) {
            return;
        }
        let viewport = window.viewport_size();
        self.zoom_at(
            point(viewport.width / 2., viewport.height / 2.),
            1. / self.zoom,
            cx,
        );
    }

    fn zoom_at(&mut self, position: Point<Pixels>, factor: f32, cx: &mut Context<Self>) {
        let old = self.zoom;
        let new = (old * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let world_x = (f32::from(position.x) - self.camera_x) / old;
        let world_y = (f32::from(position.y) - self.camera_y) / old;
        self.camera_x = f32::from(position.x) - world_x * new;
        self.camera_y = f32::from(position.y) - world_y * new;
        if self.zoom != new {
            self.last_zoom_change = std::time::Instant::now();
        }
        self.zoom = new;
        cx.notify();
    }

    pub(super) fn locate_node(&mut self, node_id: &str, window: &Window, cx: &mut Context<Self>) {
        let Some(position) = self.current_position(node_id) else {
            return;
        };
        self.overlay = Overlay::None;
        self.zoom = 1.;
        self.last_zoom_change = std::time::Instant::now();
        self.camera_x =
            f32::from(window.viewport_size().width) / 2. - (position.x + CARD_WIDTH / 2.);
        self.camera_y = 100. - position.y;
        cx.notify();
    }

    pub(super) fn render_canvas(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let viewport = window.viewport_size();
        let minimap = self.render_minimap(viewport, cx);
        let zoom_controls = self.render_zoom_controls(cx);
        let viewport_width = f32::from(viewport.width);
        let viewport_height = f32::from(viewport.height);
        let board = self.board.as_ref();
        let now = now_ms();

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
                    status_line: (node.status == NodeStatus::Running)
                        .then(|| self.running_status_line(node, now)),
                })
            })
            .collect::<Vec<_>>();

        let toolbar_buttons = self.hovered_node.as_deref().and_then(|hovered_id| {
            visible_nodes.iter().find_map(|frame| {
                let node = self.canvas_nodes.get(frame.node_index)?;
                (node.node.id == hovered_id).then(|| self.toolbar_paint_buttons(node, frame))
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
        let image_cache = self.image_cache.clone();
        let sprite_cache = self.sprite_cache.clone();
        let zoom = self.zoom;
        let zoom_settled = self.last_zoom_change.elapsed() >= ZOOM_SETTLE_DELAY;
        let camera_x = self.camera_x;
        let camera_y = self.camera_y;
        let background = canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                if !zoom_settled {
                    // Keep repainting until the settle delay elapses so the
                    // frozen sprite tiers upgrade right after the gesture.
                    let entity = window.current_view();
                    window.on_next_frame(move |_, cx| cx.notify(entity));
                }
                paint_dot_grid(bounds, camera_x, camera_y, zoom, window);
                paint_connectors(&edge_points, zoom, window);
                for frame in &visible_nodes {
                    if let Some(node) = canvas_nodes.get(frame.node_index) {
                        paint_canvas_node(
                            frame,
                            node,
                            zoom,
                            zoom_settled,
                            &image_cache,
                            &sprite_cache,
                            window,
                            cx,
                        );
                    }
                }
                if let Some(buttons) = &toolbar_buttons {
                    paint_node_toolbar(buttons, zoom, window, cx);
                }
            },
        )
        .size_full();

        let cursor = match &self.drag {
            Some(DragState::Canvas { .. }) => gpui::CursorStyle::ClosedHand,
            Some(DragState::Node { .. }) => gpui::CursorStyle::ClosedHand,
            Some(DragState::NodeClick(_)) => gpui::CursorStyle::PointingHand,
            _ if self.hovered_node.is_some() => gpui::CursorStyle::PointingHand,
            _ => gpui::CursorStyle::OpenHand,
        };
        let mut layer = div()
            .id("canvas")
            .absolute()
            .inset_0()
            .overflow_hidden()
            .bg(theme::background())
            .cursor(cursor)
            .on_scroll_wheel(
                cx.listener(|this, event: &ScrollWheelEvent, _, cx| this.scroll_canvas(event, cx)),
            )
            // Capture phase: bubble-phase pinch only fires when the canvas itself
            // is hovered, so any chrome on top would block zooming. `pinch_canvas`
            // stays inert while an overlay owns the screen.
            .capture_pinch(
                cx.listener(|this, event: &PinchEvent, _, cx| this.pinch_canvas(event, cx)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.canvas_mouse_down(event, window, cx);
                }),
            )
            .child(background);
        if let Some(minimap) = minimap {
            layer = layer.child(minimap);
        }
        layer.child(zoom_controls).into_any_element()
    }

    fn running_status_line(&self, node: &BoardNode, now: i64) -> SharedString {
        let started = node.run_started_at.unwrap_or(node.created_at);
        let activity = self
            .activity
            .get(&node.id)
            .map(String::as_str)
            .unwrap_or("Working");
        format!(
            "Generating · {}s · {activity}",
            (now - started).max(0) / 1_000
        )
        .into()
    }

    /// The zoom readout doubles as a reset button. The cluster lives in the
    /// bottom-left corner, the only edge the composer and minimap leave free.
    fn render_zoom_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded_md()
                .text_sm()
                .text_color(theme::dim())
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover()).text_color(theme::ink()))
                .child(label)
        };
        div()
            .id("zoom-controls")
            .absolute()
            .left(px(MINIMAP_RIGHT))
            .bottom(px(MINIMAP_BOTTOM))
            .flex()
            .items_center()
            .gap_1()
            .rounded_lg()
            .border_1()
            .border_color(theme::line())
            .bg(theme::raised().opacity(0.94))
            .p(px(3.))
            .block_mouse_except_scroll()
            .child(
                button("zoom-out", "−")
                    .tooltip(tip_with_shortcut("Zoom out", Some("⌘−")))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.zoom_out(&ZoomOut, window, cx)),
                    ),
            )
            .child(
                div()
                    .id("zoom-level")
                    .px_1()
                    .min_w(px(42.))
                    .text_center()
                    .text_xs()
                    .text_color(theme::dim())
                    .cursor_pointer()
                    .hover(|style| style.text_color(theme::ink()))
                    .tooltip(tip_with_shortcut("Actual size", Some("⌘0")))
                    .child(format!("{}%", (self.zoom * 100.).round() as i32))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.reset_zoom(&ResetZoom, window, cx)),
                    ),
            )
            .child(
                button("zoom-in", "+")
                    .tooltip(tip_with_shortcut("Zoom in", Some("⌘+")))
                    .on_click(cx.listener(|this, _, window, cx| this.zoom_in(&ZoomIn, window, cx))),
            )
            .child(
                button("zoom-fit", "⤢")
                    .tooltip(tip_with_shortcut("Fit board to view", Some("F")))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.fit_action(&FitCanvas, window, cx)),
                    ),
            )
            .into_any_element()
    }

    /// Cards near the top of the viewport carry their toolbar underneath, so it
    /// is never clipped off screen or buried under the board switcher.
    pub(super) fn node_toolbar_is_below(&self, screen_y: f32) -> bool {
        screen_y - NODE_TOOLBAR_HEIGHT * self.zoom < HEADER_CLEARANCE
    }

    /// Screen-space paint data for the hovered card's toolbar.
    fn toolbar_paint_buttons(
        &self,
        canvas_node: &CanvasNode,
        frame: &CanvasNodeFrame,
    ) -> Vec<ToolbarButtonPaint> {
        let running = canvas_node.node.status == NodeStatus::Running;
        let below = self.node_toolbar_is_below(frame.screen_y);
        let card_height = self.card_height(&canvas_node.node);
        toolbar_layout(running, below, card_height)
            .into_iter()
            .enumerate()
            .map(|(index, (action, rect))| ToolbarButtonPaint {
                bounds: Bounds::new(
                    point(
                        px(frame.screen_x + rect.x * self.zoom),
                        px(frame.screen_y + rect.y * self.zoom),
                    ),
                    size(px(rect.width * self.zoom), px(rect.height * self.zoom)),
                ),
                label: action.label().into(),
                color: action.color(),
                hovered: self.hovered_toolbar_button == Some(index),
            })
            .collect()
    }

    /// Which toolbar button, if any, sits under `position` on this card.
    fn toolbar_action_at(
        &self,
        canvas_node: &CanvasNode,
        position: Point<Pixels>,
        world_position: Position,
    ) -> Option<(usize, ToolbarAction)> {
        let local_x = (f32::from(position.x) - self.camera_x) / self.zoom - world_position.x;
        let local_y = (f32::from(position.y) - self.camera_y) / self.zoom - world_position.y;
        let running = canvas_node.node.status == NodeStatus::Running;
        let screen_y = self.camera_y + world_position.y * self.zoom;
        let below = self.node_toolbar_is_below(screen_y);
        let card_height = self.card_height(&canvas_node.node);
        toolbar_layout(running, below, card_height)
            .into_iter()
            .enumerate()
            .find_map(|(index, (action, rect))| {
                (local_x >= rect.x
                    && local_x <= rect.x + rect.width
                    && local_y >= rect.y
                    && local_y <= rect.y + rect.height)
                    .then_some((index, action))
            })
    }

    fn render_minimap(
        &self,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        const WIDTH: f32 = MINIMAP_WIDTH;
        const HEIGHT: f32 = MINIMAP_HEIGHT;
        const PADDING: f32 = 6.;
        const RIGHT: f32 = MINIMAP_RIGHT;
        const BOTTOM: f32 = MINIMAP_BOTTOM;

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
                .hover(|style| style.border_color(theme::faint()))
                .tooltip(tip("Click to jump the camera"))
                .block_mouse_except_scroll()
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
                // The toolbar scales with the card, so its strip matches the
                // card's width on whichever side of the card it is drawn.
                let toolbar_height = NODE_TOOLBAR_HEIGHT * self.zoom;
                let (top, bottom) = if self.node_toolbar_is_below(screen_y) {
                    (screen_y, screen_y + height + toolbar_height)
                } else {
                    (screen_y - toolbar_height, screen_y + height)
                };
                (x >= screen_x && x <= screen_x + width && y >= top && y <= bottom)
                    .then_some((index, world_position))
            })
    }

    fn canvas_click_target(
        &self,
        canvas_node: &CanvasNode,
        position: Point<Pixels>,
        world_position: Position,
    ) -> Option<CanvasClickTarget> {
        if let Some((_, action)) = self.toolbar_action_at(canvas_node, position, world_position) {
            return Some(CanvasClickTarget::Toolbar {
                node_id: canvas_node.node.id.clone(),
                action,
            });
        }
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
                let retry_top = status_y + 88. + attached_text_height(&canvas_node.node);
                (CARD_WIDTH / 2. - 29. ..=CARD_WIDTH / 2. + 29.).contains(&local_x)
                    && (retry_top..=retry_top + 26.).contains(&local_y)
            }
            NodeStatus::Error | NodeStatus::Stopped => {
                (CARD_WIDTH - 68. ..=CARD_WIDTH - 14.).contains(&local_x)
                    && (status_y..=status_y + status_area_height(&canvas_node.node))
                        .contains(&local_y)
            }
            NodeStatus::Running | NodeStatus::Done => false,
        };
        if in_retry {
            return Some(CanvasClickTarget::Retry(canvas_node.node.id.clone()));
        }
        // The status area shows only a truncated line; clicking it opens the
        // full text and error message in a popup.
        let has_message = !canvas_node.node.text.is_empty()
            || canvas_node
                .node
                .error
                .as_ref()
                .is_some_and(|e| !e.is_empty());
        if has_message
            && canvas_node.node.status != NodeStatus::Running
            && (status_y..=status_y + status_area_height(&canvas_node.node)).contains(&local_y)
        {
            return Some(CanvasClickTarget::NodeText(canvas_node.node.id.clone()));
        }
        None
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
            self.drag = match self.canvas_click_target(canvas_node, event.position, origin) {
                Some(target) => Some(DragState::NodeClick(target)),
                None => Some(DragState::Node {
                    id: canvas_node.node.id.clone(),
                    start: event.position,
                    origin,
                }),
            };
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
                let hit = self.canvas_node_at(event.position);
                let hovered = hit
                    .and_then(|(index, _)| self.canvas_nodes.get(index))
                    .map(|node| node.node.id.clone());
                let hovered_button = hit.and_then(|(index, world_position)| {
                    let canvas_node = self.canvas_nodes.get(index)?;
                    self.toolbar_action_at(canvas_node, event.position, world_position)
                        .map(|(button_index, _)| button_index)
                });
                if hovered != self.hovered_node || hovered_button != self.hovered_toolbar_button {
                    self.hovered_node = hovered;
                    self.hovered_toolbar_button = hovered_button;
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
        if let Some(DragState::Node { id, .. }) = drag {
            if let Some(position) = self.transient_positions.remove(&id) {
                self.on_board(cx, |this, board_id| {
                    // Pin every still-automatic card where it stands, not just
                    // the dragged one. Otherwise the tree layout re-centres the
                    // remaining cards and they flow into the space the user
                    // just cleared.
                    let mut positions: Vec<(String, f32, f32)> = this
                        .board
                        .iter()
                        .flat_map(|board| &board.nodes)
                        .filter(|node| node.id != id && (node.x.is_none() || node.y.is_none()))
                        .filter_map(|node| {
                            let current = this.layout.get(&node.id)?;
                            Some((node.id.clone(), current.x, current.y))
                        })
                        .collect();
                    positions.push((id.clone(), position.x, position.y));
                    this.engine.repository().move_nodes(board_id, &positions)?;
                    // The repository confirms the move through an async event a
                    // few frames from now; apply it to the local copy too so no
                    // card falls back to its pre-drag layout slot.
                    if let Some(board) = &mut this.board {
                        for (id, x, y) in &positions {
                            if let Some(node) = board.nodes.iter_mut().find(|node| &node.id == id) {
                                node.x = Some(*x);
                                node.y = Some(*y);
                            }
                        }
                    }
                    this.refresh_layout();
                    Ok(())
                });
            }
        } else if let Some(DragState::NodeClick(click_target)) = drag {
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
                CanvasClickTarget::NodeText(node_id) => {
                    self.overlay = Overlay::NodeText(node_id);
                }
                CanvasClickTarget::Toolbar { node_id, action } => {
                    self.run_toolbar_action(&node_id, action, window, cx)
                }
            }
        }
        cx.notify();
    }

    fn run_toolbar_action(
        &mut self,
        node_id: &str,
        action: ToolbarAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            ToolbarAction::Stop => self.engine.stop_node(node_id),
            ToolbarAction::Branch => self.branch_node(node_id, None, window, cx),
            ToolbarAction::Edit => self.edit_node(node_id, window, cx),
            ToolbarAction::Retry => self.regenerate_node(node_id, cx),
            ToolbarAction::Copy => {
                if let Some(node) = self.node(node_id) {
                    cx.write_to_clipboard(ClipboardItem::new_string(node.prompt));
                    self.show_toast("Prompt copied".into(), false, None, cx);
                }
            }
            ToolbarAction::Duplicate => self.duplicate_node(node_id, cx),
            ToolbarAction::Delete => self.delete_node(node_id, cx),
        }
    }
}
