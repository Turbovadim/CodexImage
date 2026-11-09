//! The full-screen image viewer: its zoom and pan state, keyboard navigation
//! between the images of a board, and the toolbar layered over the image.

use super::app::AppView;
use super::app::Overlay;
use super::canvas_view::DragState;
use super::composer::control_button;
use super::format::image_format_for_path;
use super::image_cache::DECODED_LONG_EDGE_CAP;
use super::keymap::{LightboxDown, LightboxLeft, LightboxRight, LightboxUp, ToggleCompare};
use super::theme;
use super::tooltip::tip_with_shortcut;
use crate::model::{Board, BoardNode};
use gpui::{
    AnyElement, ClipboardItem, Context, Image, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, PinchEvent, Pixels, Point, Resource, Role, ScrollEvent, StyledImage,
    Window, div, img, prelude::*, px,
};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) const LIGHTBOX_MIN_ZOOM: f32 = 1.;

pub(super) const LIGHTBOX_MAX_ZOOM: f32 = 8.;
/// The area the image is fitted and panned within.
fn lightbox_stage_size(window: &Window) -> (f32, f32) {
    let viewport = window.viewport_size();
    (f32::from(viewport.width), f32::from(viewport.height))
}

pub(super) struct Lightbox {
    pub(super) node_id: String,
    pub(super) image: String,
    pub(super) zoom: f32,
    pub(super) pan_x: f32,
    pub(super) pan_y: f32,
    pub(super) pending: Option<LightboxLocation>,
    pub(super) memory: LightboxMemory,
    /// Where the before/after divider sits, as a fraction of the image width.
    /// `None` while not comparing.
    pub(super) compare: Option<f32>,
}

/// Where vertical navigation returns to: the image last shown on each card,
/// and the card below each card that the user last moved through. Left/right
/// updates the image memory, so up then down comes back to the swapped image.
#[derive(Default)]
pub(super) struct LightboxMemory {
    last_image: HashMap<String, String>,
    descent: HashMap<String, String>,
}

impl LightboxMemory {
    fn remember(&mut self, location: &LightboxLocation) {
        self.last_image
            .insert(location.node_id.clone(), location.image.clone());
    }

    fn image_for<'a>(&'a self, node: &'a BoardNode) -> Option<&'a String> {
        self.last_image
            .get(&node.id)
            .filter(|image| node.images.contains(image))
            .or_else(|| node.images.first())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LightboxLocation {
    pub(super) node_id: String,
    pub(super) image: String,
}

impl Lightbox {
    fn displayed_location(&self) -> LightboxLocation {
        LightboxLocation {
            node_id: self.node_id.clone(),
            image: self.image.clone(),
        }
    }

    fn navigation_origin(&self) -> LightboxLocation {
        self.pending
            .clone()
            .unwrap_or_else(|| self.displayed_location())
    }

    fn request(&mut self, target: LightboxLocation) {
        self.pending = (target != self.displayed_location()).then_some(target);
    }

    fn commit_pending(&mut self, expected: &LightboxLocation) -> bool {
        let Some(target) = self.pending.take_if(|pending| pending == expected) else {
            return false;
        };
        self.memory.remember(&target);
        self.node_id = target.node_id;
        self.image = target.image;
        self.reset_view();
        true
    }

    fn reset_view(&mut self) {
        self.zoom = LIGHTBOX_MIN_ZOOM;
        self.pan_x = 0.;
        self.pan_y = 0.;
    }

    fn zoom_at(
        &mut self,
        factor: f32,
        focal: Point<Pixels>,
        viewport_width: f32,
        viewport_height: f32,
        image_ratio: f32,
    ) {
        if !factor.is_finite() || factor <= 0. {
            return;
        }
        let previous_zoom = self.zoom;
        let next_zoom = (previous_zoom * factor).clamp(LIGHTBOX_MIN_ZOOM, LIGHTBOX_MAX_ZOOM);
        let zoom_ratio = next_zoom / previous_zoom;
        let focal_x = f32::from(focal.x) - viewport_width / 2.;
        let focal_y = f32::from(focal.y) - viewport_height / 2.;
        self.pan_x += (focal_x - self.pan_x) * (1. - zoom_ratio);
        self.pan_y += (focal_y - self.pan_y) * (1. - zoom_ratio);
        self.zoom = next_zoom;
        if self.zoom <= LIGHTBOX_MIN_ZOOM {
            self.reset_view();
        } else {
            self.clamp_pan(viewport_width, viewport_height, image_ratio);
        }
    }

    fn pan_to(
        &mut self,
        pan_x: f32,
        pan_y: f32,
        viewport_width: f32,
        viewport_height: f32,
        image_ratio: f32,
    ) {
        self.pan_x = pan_x;
        self.pan_y = pan_y;
        self.clamp_pan(viewport_width, viewport_height, image_ratio);
    }

    fn clamped_pan(
        &self,
        viewport_width: f32,
        viewport_height: f32,
        image_ratio: f32,
    ) -> (f32, f32) {
        let (fit_width, fit_height) = fitted_image_size(
            viewport_width,
            viewport_height,
            normalized_image_ratio(image_ratio),
        );
        let max_x = ((fit_width * self.zoom - viewport_width) / 2.).max(0.);
        let max_y = ((fit_height * self.zoom - viewport_height) / 2.).max(0.);
        (
            self.pan_x.clamp(-max_x, max_x),
            self.pan_y.clamp(-max_y, max_y),
        )
    }

    fn clamp_pan(&mut self, viewport_width: f32, viewport_height: f32, image_ratio: f32) {
        (self.pan_x, self.pan_y) = self.clamped_pan(viewport_width, viewport_height, image_ratio);
    }

    /// The displayed image's left, top, width and height within the stage.
    fn image_rect(
        &self,
        viewport_width: f32,
        viewport_height: f32,
        image_ratio: f32,
    ) -> (f32, f32, f32, f32) {
        let (fit_width, fit_height) = fitted_image_size(
            viewport_width,
            viewport_height,
            normalized_image_ratio(image_ratio),
        );
        let (width, height) = (fit_width * self.zoom, fit_height * self.zoom);
        let (pan_x, pan_y) = self.clamped_pan(viewport_width, viewport_height, image_ratio);
        (
            (viewport_width - width) / 2. + pan_x,
            (viewport_height - height) / 2. + pan_y,
            width,
            height,
        )
    }
}

pub(super) fn normalized_image_ratio(image_ratio: f32) -> f32 {
    if image_ratio.is_finite() && image_ratio > 0. {
        image_ratio
    } else {
        1.
    }
}

pub(super) fn fitted_image_size(
    viewport_width: f32,
    viewport_height: f32,
    image_ratio: f32,
) -> (f32, f32) {
    let viewport_width = viewport_width.max(0.);
    let viewport_height = viewport_height.max(0.);
    if viewport_width == 0. || viewport_height == 0. {
        return (0., 0.);
    }
    if viewport_width / viewport_height > image_ratio {
        (viewport_height * image_ratio, viewport_height)
    } else {
        (viewport_width, viewport_width / image_ratio)
    }
}

pub(super) fn lightbox_target(
    board: &Board,
    memory: &LightboxMemory,
    current: &LightboxLocation,
    horizontal: i32,
    vertical: i32,
) -> Option<LightboxLocation> {
    if horizontal == 0 && vertical == 0 {
        return None;
    }
    let node = board.nodes.iter().find(|node| node.id == current.node_id)?;
    if horizontal != 0 {
        let images = node.image_set(&current.image);
        let index = images
            .iter()
            .position(|image| image == &current.image)
            .unwrap_or(0) as i32
            + horizontal;
        if index >= 0 && (index as usize) < images.len() {
            return Some(LightboxLocation {
                node_id: node.id.clone(),
                image: images[index as usize].clone(),
            });
        }
        // Reference images are browsed on their own and never hop cards.
        if node.is_reference(&current.image) {
            return None;
        }
        // Past the card's edge, only single-image cards hop between each
        // other. Multi-image cards stay self-contained so left/right never
        // leaves them, and they are never landed on from a sibling.
        // Siblings share a parent; top-level cards share having none.
        // Horizontal navigation runs during both render and prefetch. Select
        // the neighboring sort key in-place instead of allocating and sorting
        // every sibling on each call.
        if node.images.len() != 1 {
            return None;
        }
        let mut sibling = node;
        for _ in 0..horizontal.unsigned_abs() {
            let key = (sibling.created_at, sibling.id.as_str());
            sibling = if horizontal > 0 {
                board
                    .nodes
                    .iter()
                    .filter(|candidate| {
                        candidate.parent_id == node.parent_id
                            && candidate.images.len() == 1
                            && (candidate.created_at, candidate.id.as_str()) > key
                    })
                    .min_by_key(|candidate| (candidate.created_at, candidate.id.as_str()))?
            } else {
                board
                    .nodes
                    .iter()
                    .filter(|candidate| {
                        candidate.parent_id == node.parent_id
                            && candidate.images.len() == 1
                            && (candidate.created_at, candidate.id.as_str()) < key
                    })
                    .max_by_key(|candidate| (candidate.created_at, candidate.id.as_str()))?
            };
        }
        let image = if horizontal > 0 {
            sibling.images.first()
        } else {
            sibling.images.last()
        }?;
        return Some(LightboxLocation {
            node_id: sibling.id.clone(),
            image: image.clone(),
        });
    }
    if vertical < 0 {
        let mut parent_id = node.parent_id.as_deref();
        while let Some(id) = parent_id {
            let parent = board.nodes.iter().find(|candidate| candidate.id == id)?;
            if let Some(image) = memory.image_for(parent) {
                return Some(LightboxLocation {
                    node_id: parent.id.clone(),
                    image: image.clone(),
                });
            }
            parent_id = parent.parent_id.as_deref();
        }
        return None;
    }

    // Return to the card the user last came up from, if it is still below.
    let remembered = memory
        .descent
        .get(&node.id)
        .and_then(|id| board.nodes.iter().find(|candidate| &candidate.id == id))
        .filter(|candidate| is_descendant(board, candidate, &node.id))
        .and_then(|child| Some((child, memory.image_for(child)?)));
    if let Some((child, image)) = remembered {
        return Some(LightboxLocation {
            node_id: child.id.clone(),
            image: image.clone(),
        });
    }
    let mut queue: VecDeque<&BoardNode> = board
        .nodes
        .iter()
        .filter(|candidate| candidate.parent_id.as_deref() == Some(&node.id))
        .collect();
    while let Some(child) = queue.pop_front() {
        if let Some(image) = memory.image_for(child) {
            return Some(LightboxLocation {
                node_id: child.id.clone(),
                image: image.clone(),
            });
        }
        queue.extend(
            board
                .nodes
                .iter()
                .filter(|candidate| candidate.parent_id.as_deref() == Some(&child.id)),
        );
    }
    None
}

fn is_descendant(board: &Board, node: &BoardNode, ancestor_id: &str) -> bool {
    let mut parent_id = node.parent_id.as_deref();
    while let Some(id) = parent_id {
        if id == ancestor_id {
            return true;
        }
        parent_id = board
            .nodes
            .iter()
            .find(|candidate| candidate.id == id)
            .and_then(|parent| parent.parent_id.as_deref());
    }
    false
}

impl AppView {
    pub(super) fn navigate_left(
        &mut self,
        _: &LightboxLeft,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_lightbox(-1, 0, cx)
    }

    pub(super) fn navigate_right(
        &mut self,
        _: &LightboxRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_lightbox(1, 0, cx)
    }

    pub(super) fn navigate_up(&mut self, _: &LightboxUp, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate_lightbox(0, -1, cx)
    }

    pub(super) fn navigate_down(
        &mut self,
        _: &LightboxDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_lightbox(0, 1, cx)
    }

    pub(super) fn open_lightbox(
        &mut self,
        node_id: String,
        image: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut memory = LightboxMemory::default();
        memory.remember(&LightboxLocation {
            node_id: node_id.clone(),
            image: image.clone(),
        });
        self.overlay = Overlay::Lightbox(Lightbox {
            node_id,
            image,
            zoom: LIGHTBOX_MIN_ZOOM,
            pan_x: 0.,
            pan_y: 0.,
            pending: None,
            memory,
            compare: None,
        });
        window.focus(&self.lightbox_focus, cx);
        cx.notify();
    }

    fn navigate_lightbox(&mut self, horizontal: i32, vertical: i32, cx: &mut Context<Self>) {
        let Overlay::Lightbox(current) = &self.overlay else {
            return;
        };
        let Some(board) = &self.board else { return };
        let origin = current.navigation_origin();
        let Some(target) = lightbox_target(board, &current.memory, &origin, horizontal, vertical)
        else {
            return;
        };
        let Overlay::Lightbox(current) = &mut self.overlay else {
            return;
        };
        if vertical < 0 {
            current
                .memory
                .descent
                .insert(target.node_id.clone(), origin.node_id);
        } else if vertical > 0 {
            current
                .memory
                .descent
                .insert(origin.node_id, target.node_id.clone());
        }
        current.request(target);
        cx.notify();
    }

    pub(super) fn prepare_lightbox_assets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = match &self.overlay {
            Overlay::Lightbox(lightbox) => lightbox.pending.clone(),
            _ => return,
        };
        let mut load_error = None;
        if let Some(pending) = pending {
            let resource = Resource::Path(Arc::from(self.display_image_path(&pending.image, true)));
            match self.image_cache.update(cx, |cache, cx| {
                cache.load(&resource, DECODED_LONG_EDGE_CAP, window, cx)
            }) {
                Some(Ok(image)) if image.frame_count() > 0 => {
                    if let Overlay::Lightbox(lightbox) = &mut self.overlay {
                        lightbox.commit_pending(&pending);
                    }
                }
                Some(Ok(_)) => load_error = Some("Image contains no displayable frames".to_owned()),
                Some(Err(error)) => load_error = Some(format!("Could not load image: {error}")),
                // Do not fan out more neighbor work while the image the user
                // explicitly requested is still waiting for a decode slot.
                None => return,
            }
        }
        if let Some(error) = load_error {
            if let Overlay::Lightbox(lightbox) = &mut self.overlay {
                lightbox.pending = None;
            }
            self.show_error(error, cx);
        }

        let Some((board, lightbox)) = self.board.as_ref().and_then(|board| {
            let Overlay::Lightbox(lightbox) = &self.overlay else {
                return None;
            };
            Some((board, lightbox))
        }) else {
            return;
        };
        let current = lightbox.displayed_location();
        let mut previous_path = None;
        for horizontal in [-1, 1] {
            let Some(target) = lightbox_target(board, &lightbox.memory, &current, horizontal, 0)
            else {
                continue;
            };
            let path = self.display_image_path(&target.image, true);
            if previous_path.as_ref() == Some(&path) {
                continue;
            }
            previous_path = Some(path.clone());
            let resource = Resource::Path(Arc::from(path));
            self.image_cache
                .update(cx, |cache, cx| cache.prefetch(&resource, window, cx));
        }
    }

    /// The aspect ratio of the image the lightbox is showing, if it is open.
    fn lightbox_image_ratio(&self) -> Option<f32> {
        let Overlay::Lightbox(lightbox) = &self.overlay else {
            return None;
        };
        Some(
            self.image_ratios
                .get(&lightbox.image)
                .copied()
                .unwrap_or(1.),
        )
    }

    fn zoom_lightbox(
        &mut self,
        factor: f32,
        focal: Point<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(image_ratio) = self.lightbox_image_ratio() else {
            return;
        };
        let (stage_width, stage_height) = lightbox_stage_size(window);
        if let Overlay::Lightbox(lightbox) = &mut self.overlay {
            lightbox.zoom_at(factor, focal, stage_width, stage_height, image_ratio);
            cx.notify();
        }
    }

    /// The image the open lightbox image was made from, if it has one.
    fn lightbox_compare_source(&self) -> Option<&str> {
        let Overlay::Lightbox(lightbox) = &self.overlay else {
            return None;
        };
        self.board
            .as_ref()?
            .nodes
            .iter()
            .find(|node| node.id == lightbox.node_id)?
            .compare_source(&lightbox.image)
    }

    pub(super) fn toggle_compare(
        &mut self,
        _: &ToggleCompare,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let has_source = self.lightbox_compare_source().is_some();
        if let Overlay::Lightbox(lightbox) = &mut self.overlay
            && has_source
        {
            lightbox.compare = match lightbox.compare {
                Some(_) => None,
                None => Some(0.5),
            };
            cx.notify();
        }
    }

    /// While comparing, the divider follows the pointer across the image.
    fn move_compare_divider(&mut self, x: Pixels, window: &Window, cx: &mut Context<Self>) {
        let Some(image_ratio) = self.lightbox_image_ratio() else {
            return;
        };
        let (stage_width, stage_height) = lightbox_stage_size(window);
        if let Overlay::Lightbox(lightbox) = &mut self.overlay
            && lightbox.compare.is_some()
        {
            let (left, _, width, _) = lightbox.image_rect(stage_width, stage_height, image_ratio);
            if width > 0. {
                lightbox.compare = Some(((f32::from(x) - left) / width).clamp(0., 1.));
                cx.notify();
            }
        }
    }

    fn lightbox_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(DragState::Lightbox { start, origin }) = &self.drag else {
            self.move_compare_divider(event.position.x, window, cx);
            return;
        };
        if !event.dragging() {
            return;
        }
        let start = *start;
        let origin = *origin;
        let Some(image_ratio) = self.lightbox_image_ratio() else {
            return;
        };
        let (stage_width, stage_height) = lightbox_stage_size(window);
        if let Overlay::Lightbox(lightbox) = &mut self.overlay {
            lightbox.pan_to(
                origin.0 + f32::from(event.position.x - start.x),
                origin.1 + f32::from(event.position.y - start.y),
                stage_width,
                stage_height,
                image_ratio,
            );
            cx.notify();
        }
    }

    fn lightbox_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(&self.drag, Some(DragState::Lightbox { .. })) {
            self.drag = None;
            cx.notify();
        }
    }

    pub(super) fn render_lightbox(
        &self,
        lightbox: &Lightbox,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = self.display_image_path(&lightbox.image, true);
        let thumbnail_path = self.display_image_path(&lightbox.image, false);
        let resource = Resource::Path(Arc::from(path.clone()));
        let (viewport_width, viewport_height) = lightbox_stage_size(window);
        let image_ratio = normalized_image_ratio(
            self.image_ratios
                .get(&lightbox.image)
                .copied()
                .unwrap_or(1.),
        );
        let (image_left, image_top, image_width, image_height) =
            lightbox.image_rect(viewport_width, viewport_height, image_ratio);
        // Past this displayed size the capped decode would look soft, so ask
        // for the native pixels. The capped decode (or the thumbnail) stays on
        // screen until the sharper tier finishes loading. A source that fits
        // the cap was never downscaled, so its capped decode already is native.
        let source_exceeds_cap = self
            .board
            .as_ref()
            .and_then(|board| board.image_sizes.get(&lightbox.image))
            .is_none_or(|size| size[0].max(size[1]) > DECODED_LONG_EDGE_CAP);
        let wants_native = source_exceeds_cap
            && image_width.max(image_height) * window.scale_factor() > DECODED_LONG_EDGE_CAP as f32;
        let native = wants_native
            .then(|| {
                self.image_cache
                    .update(cx, |cache, cx| cache.load_full(&resource, window, cx))
            })
            .flatten();
        let native = native
            .and_then(Result::ok)
            .filter(|image| image.frame_count() > 0);
        // Once the native tier is ready, stop touching the capped tier. It is
        // no longer visible and can then participate in the next LRU trim.
        let display_image = if let Some(image) = native {
            img(image)
        } else {
            match self.image_cache.update(cx, |cache, cx| {
                cache.load(&resource, DECODED_LONG_EDGE_CAP, window, cx)
            }) {
                Some(Ok(image)) if image.frame_count() > 0 => img(image),
                _ => img(thumbnail_path),
            }
        };
        let node = self
            .board
            .as_ref()
            .and_then(|board| board.nodes.iter().find(|node| node.id == lightbox.node_id));
        let images = node.map(|node| node.image_set(&lightbox.image));
        let is_reference = node.is_some_and(|node| node.is_reference(&lightbox.image));
        let image_index = images
            .and_then(|images| images.iter().position(|image| image == &lightbox.image))
            .unwrap_or(0);
        let total = images.map_or(1, <[String]>::len);
        let copy_path = path.clone();
        let save_path = path.clone();
        let open_path = path.clone();
        let branch_node = lightbox.node_id.clone();
        let branch_image = lightbox.image.clone();
        let combine_node = lightbox.node_id.clone();
        let combine_image = lightbox.image.clone();
        let locate_node = lightbox.node_id.clone();
        let compare_source = node.and_then(|node| node.compare_source(&lightbox.image));
        let compare = lightbox.compare.zip(compare_source);
        let lightbox_dragging = matches!(&self.drag, Some(DragState::Lightbox { .. }));
        let stage_cursor = if lightbox_dragging {
            gpui::CursorStyle::ClosedHand
        } else if lightbox.zoom > LIGHTBOX_MIN_ZOOM {
            gpui::CursorStyle::OpenHand
        } else if compare.is_some() {
            gpui::CursorStyle::ResizeLeftRight
        } else {
            gpui::CursorStyle::Arrow
        };
        let mut toolbar = div()
            .absolute()
            .top(px(16.))
            .right(px(16.))
            .flex()
            .gap_2()
            .occlude();
        if compare_source.is_some() {
            toolbar = toolbar.child(control_button(
                if compare.is_some() {
                    "Done comparing (X)"
                } else {
                    "Compare (X)"
                },
                cx.listener(|this, _, window, cx| this.toggle_compare(&ToggleCompare, window, cx)),
            ));
        }
        let toolbar = toolbar
            .child(control_button(
                "Branch (B)",
                cx.listener(move |this, _, window, cx| {
                    this.branch_node(&branch_node, Some(branch_image.clone()), window, cx);
                }),
            ))
            .child(control_button(
                "Combine (C)",
                cx.listener(move |this, _, window, cx| {
                    this.combine_node(&combine_node, Some(combine_image.clone()), window, cx);
                }),
            ))
            .child(control_button(
                "Show on canvas",
                cx.listener(move |this, _, window, cx| this.locate_node(&locate_node, window, cx)),
            ))
            .child(control_button(
                "Copy",
                cx.listener(move |this, _, _, cx| this.copy_image(&copy_path, cx)),
            ))
            .child(control_button(
                "Save…",
                cx.listener(move |this, _, _, cx| this.save_image(save_path.clone(), cx)),
            ))
            .child(control_button(
                "Open original",
                cx.listener(move |_, _, _, _| {
                    let path = open_path.clone();
                    smol::unblock(move || {
                        let _ = crate::platform::open_path(&path);
                    })
                    .detach();
                }),
            ))
            .child(control_button(
                "×",
                cx.listener(|this, _, window, cx| {
                    this.close_overlay(window, cx);
                }),
            ));

        let mut root = div()
            .id("lightbox")
            .role(Role::Dialog)
            .aria_label("Image lightbox")
            .absolute()
            .inset_0()
            .key_context("CodexImage CodexImageLightbox")
            .track_focus(&self.lightbox_focus)
            .overflow_hidden()
            .bg(gpui::black().opacity(0.97))
            .occlude()
            .on_scroll(cx.listener(|this, event: &ScrollEvent, window, cx| {
                let delta = event.delta.pixel_delta(px(18.));
                let factor = (-f32::from(delta.y) * 0.004).exp();
                this.zoom_lightbox(factor, event.position, window, cx);
            }))
            .on_pinch(cx.listener(|this, event: &PinchEvent, window, cx| {
                this.zoom_lightbox(1. + event.delta, event.position, window, cx);
            }))
            .on_mouse_move(cx.listener(Self::lightbox_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::lightbox_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::lightbox_mouse_up))
            .child(
                div()
                    .id("lightbox-stage")
                    .absolute()
                    .inset_0()
                    .overflow_hidden()
                    .cursor(stage_cursor)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            window.focus(&this.lightbox_focus, cx);
                            if let Overlay::Lightbox(lightbox) = &this.overlay
                                && lightbox.zoom > LIGHTBOX_MIN_ZOOM
                            {
                                this.drag = Some(DragState::Lightbox {
                                    start: event.position,
                                    origin: (lightbox.pan_x, lightbox.pan_y),
                                });
                                cx.notify();
                            }
                        }),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        if matches!(&this.overlay, Overlay::Lightbox(lightbox)
                            if lightbox.zoom <= 1. && lightbox.compare.is_none())
                        {
                            this.close_overlay(window, cx);
                        }
                    }))
                    .child(match compare {
                        None => display_image
                            .absolute()
                            .left(px(image_left))
                            .top(px(image_top))
                            .w(px(image_width))
                            .h(px(image_height))
                            .object_fit(ObjectFit::Contain)
                            .into_any_element(),
                        Some((split, source)) => self.render_comparison(
                            display_image,
                            source,
                            split,
                            (image_left, image_top, image_width, image_height),
                            window,
                            cx,
                        ),
                    }),
            )
            .child(toolbar);
        let location = lightbox.displayed_location();
        for (direction, id, glyph, hint) in [
            (-1, "lightbox-previous", "‹", "Previous image"),
            (1, "lightbox-next", "›", "Next image"),
        ] {
            if self
                .board
                .as_ref()
                .and_then(|board| lightbox_target(board, &lightbox.memory, &location, direction, 0))
                .is_none()
            {
                continue;
            }
            let mut chevron = div()
                .id(id)
                .role(Role::Button)
                .aria_label(hint)
                .absolute()
                .top(px(viewport_height / 2. - 26.))
                .size(px(52.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(theme::line())
                .bg(theme::raised().opacity(0.82))
                .text_size(px(26.))
                .text_color(theme::dim())
                .cursor_pointer()
                .occlude()
                .hover(|style| style.bg(theme::hover()).text_color(theme::ink()))
                .tooltip(tip_with_shortcut(
                    hint,
                    Some(if direction < 0 { "←" } else { "→" }),
                ))
                .child(glyph)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.navigate_lightbox(direction, 0, cx);
                }));
            chevron = if direction < 0 {
                chevron.left(px(18.))
            } else {
                chevron.right(px(18.))
            };
            root = root.child(chevron);
        }
        if total > 1 || is_reference {
            root = root.child(
                div()
                    .id("lightbox-position")
                    .absolute()
                    .top(px(16.))
                    .left(px(16.))
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::line())
                    .bg(theme::raised().opacity(0.9))
                    .px_3()
                    .py_2()
                    .role(Role::Label)
                    .aria_label(format!("Image {} of {}", image_index + 1, total))
                    .text_sm()
                    .text_color(theme::ink())
                    .child(if is_reference {
                        format!("Reference {} / {}", image_index + 1, total)
                    } else {
                        format!("{} / {}", image_index + 1, total)
                    }),
            );
        }
        if lightbox.zoom > 1. {
            root = root.child(
                div()
                    .absolute()
                    .left(px(20.))
                    .bottom(px(20.))
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::line())
                    .bg(theme::raised().opacity(0.9))
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(theme::dim())
                    .child(format!("{}%", (lightbox.zoom * 100.).round() as i32)),
            );
        }
        root.into_any_element()
    }

    /// The source image under the result, with the result clipped to the right
    /// of a divider at `split` (a fraction of the image width). The source is
    /// fitted into the result's frame, so differing aspect ratios still line up
    /// edge to edge.
    fn render_comparison(
        &self,
        result: gpui::Img,
        source: &str,
        split: f32,
        (left, top, width, height): (f32, f32, f32, f32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let resource = Resource::Path(Arc::from(self.display_image_path(source, true)));
        let before = match self.image_cache.update(cx, |cache, cx| {
            cache.load(&resource, DECODED_LONG_EDGE_CAP, window, cx)
        }) {
            Some(Ok(image)) if image.frame_count() > 0 => img(image),
            _ => img(self.display_image_path(source, false)),
        };
        let (viewport_width, viewport_height) = lightbox_stage_size(window);
        let divider = width * split;
        let label_top = (top + height).min(viewport_height) - 44.;
        let label = |text: &'static str| {
            div()
                .absolute()
                .top(px(label_top))
                .rounded_md()
                .bg(gpui::black().opacity(0.6))
                .px_2()
                .py_1()
                .text_xs()
                .text_color(gpui::white())
                .child(text)
        };
        div()
            .absolute()
            .inset_0()
            .child(
                before
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(width))
                    .h(px(height))
                    .object_fit(ObjectFit::Contain),
            )
            .child(
                div()
                    .absolute()
                    .left(px(left + divider))
                    .top(px(top))
                    .w(px(width - divider))
                    .h(px(height))
                    .overflow_hidden()
                    .child(
                        result
                            .absolute()
                            .left(px(-divider))
                            .top_0()
                            .w(px(width))
                            .h(px(height))
                            .object_fit(ObjectFit::Contain),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .left(px(left + divider - 1.))
                    .top(px(top))
                    .w(px(2.))
                    .h(px(height))
                    .bg(gpui::white().opacity(0.85)),
            )
            .when(split > 0.08, |stage| {
                stage.child(label("Original").right(px(viewport_width - (left + divider) + 10.)))
            })
            .when(split < 0.92, |stage| {
                stage.child(label("Result").left(px(left + divider + 10.)))
            })
            .into_any_element()
    }

    /// Copies the full-resolution original. Originals run to several megabytes,
    /// so the read happens off the main thread; only the clipboard write, which
    /// is a platform call, comes back to it.
    fn copy_image(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(format) = image_format_for_path(path) else {
            self.show_error("Unsupported image format", cx);
            return;
        };
        let path = path.to_owned();
        cx.spawn(async move |weak, cx| {
            let bytes = smol::unblock(move || fs::read(path)).await;
            let _ = weak.update(cx, |view, cx| match bytes {
                Ok(bytes) => {
                    cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(
                        format, bytes,
                    )));
                    view.show_toast("Image copied".into(), false, None, cx);
                }
                Err(error) => view.show_error(error, cx),
            });
        })
        .detach();
    }

    fn save_image(&mut self, source: PathBuf, cx: &mut Context<Self>) {
        let suggested = source
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "image.png".into());
        let directory = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| self.engine.repository().paths().root.clone());
        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested));
        cx.spawn(async move |weak, cx| {
            let Ok(Ok(Some(destination))) = receiver.await else {
                return;
            };
            let result = smol::unblock(move || fs::copy(source, destination)).await;
            let _ = weak.update(cx, |view, cx| match result {
                Ok(_) => view.show_toast("Image saved".into(), false, None, cx),
                Err(error) => view.show_error(error, cx),
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::{Lightbox, LightboxLocation, LightboxMemory, lightbox_target};
    use crate::model::{Board, BoardNode, NodeStatus};
    use gpui::{point, px};

    fn tree_node(id: &str, parent_id: Option<&str>, images: &[&str], created_at: i64) -> BoardNode {
        BoardNode {
            id: id.into(),
            parent_id: parent_id.map(str::to_owned),
            merged_from: Vec::new(),
            prompt: "prompt".into(),
            aspect: "auto".into(),
            source_images: Vec::new(),
            attachments: Vec::new(),
            images: images.iter().map(|image| (*image).to_owned()).collect(),
            image_labels: Vec::new(),
            attempts: Vec::new(),
            text: String::new(),
            status: NodeStatus::Done,
            error: None,
            stop_reason: None,
            x: None,
            y: None,
            created_at,
            run_started_at: None,
            finished_at: None,
            usage: None,
            chat: Vec::new(),
        }
    }

    #[test]
    fn comparison_prefers_the_edited_image_over_attachments() {
        let mut node = tree_node("node", None, &["out.png"], 0);
        assert_eq!(node.compare_source("out.png"), None);
        node.attachments = vec!["ref.png".into()];
        assert_eq!(node.compare_source("out.png"), Some("ref.png"));
        assert_eq!(node.compare_source("ref.png"), None);
        node.source_images = vec!["parent.png".into()];
        assert_eq!(node.compare_source("out.png"), Some("parent.png"));
    }

    #[test]
    fn lightbox_keeps_the_displayed_image_until_a_pending_image_is_committed() {
        let mut lightbox = Lightbox {
            node_id: "node-a".into(),
            image: "a.png".into(),
            zoom: 3.,
            pan_x: 40.,
            pan_y: -20.,
            pending: None,
            memory: LightboxMemory::default(),
            compare: None,
        };
        let destination = LightboxLocation {
            node_id: "node-b".into(),
            image: "b.png".into(),
        };

        lightbox.request(destination.clone());
        assert_eq!(lightbox.displayed_location().image, "a.png");
        assert_eq!(lightbox.navigation_origin(), destination);
        assert_eq!(lightbox.zoom, 3.);

        assert!(lightbox.commit_pending(&destination));
        assert_eq!(lightbox.displayed_location(), destination);
        assert_eq!(lightbox.zoom, 1.);
        assert_eq!((lightbox.pan_x, lightbox.pan_y), (0., 0.));
        assert!(lightbox.pending.is_none());
    }

    #[test]
    fn lightbox_gesture_zoom_scales_and_clamps_safely() {
        let mut lightbox = Lightbox {
            node_id: "node-a".into(),
            image: "a.png".into(),
            zoom: 1.,
            pan_x: 0.,
            pan_y: 0.,
            pending: None,
            memory: LightboxMemory::default(),
            compare: None,
        };

        lightbox.zoom_at(1.5, point(px(500.), px(400.)), 1_000., 800., 1.);
        assert_eq!(lightbox.zoom, 1.5);
        lightbox.zoom_at(0.5, point(px(500.), px(400.)), 1_000., 800., 1.);
        assert_eq!(lightbox.zoom, 1.);
        lightbox.zoom_at(20., point(px(500.), px(400.)), 1_000., 800., 1.);
        assert_eq!(lightbox.zoom, 8.);
        lightbox.zoom_at(f32::NAN, point(px(500.), px(400.)), 1_000., 800., 1.);
        assert_eq!(lightbox.zoom, 8.);
    }

    #[test]
    fn lightbox_zoom_preserves_the_image_point_beneath_the_gesture() {
        let mut lightbox = Lightbox {
            node_id: "node-a".into(),
            image: "a.png".into(),
            zoom: 1.,
            pan_x: 0.,
            pan_y: 0.,
            pending: None,
            memory: LightboxMemory::default(),
            compare: None,
        };
        let focal_x = 200.;
        let image_point_before = (focal_x - lightbox.pan_x) / lightbox.zoom;

        lightbox.zoom_at(2., point(px(700.), px(400.)), 1_000., 800., 1.);

        let image_point_after = (focal_x - lightbox.pan_x) / lightbox.zoom;
        assert_eq!(lightbox.zoom, 2.);
        assert_eq!(lightbox.pan_x, -200.);
        assert_eq!(image_point_before, image_point_after);
    }

    #[test]
    fn lightbox_drag_pan_is_clamped_to_the_visible_image_bounds() {
        let mut lightbox = Lightbox {
            node_id: "node-a".into(),
            image: "a.png".into(),
            zoom: 2.,
            pan_x: 0.,
            pan_y: 0.,
            pending: None,
            memory: LightboxMemory::default(),
            compare: None,
        };

        lightbox.pan_to(999., -999., 1_000., 800., 2.);

        assert_eq!(lightbox.pan_x, 500.);
        assert_eq!(lightbox.pan_y, -100.);
    }

    #[test]
    fn lightbox_navigation_resolves_destinations_without_changing_the_displayed_image() {
        let board = Board {
            id: "board".into(),
            title: "Board".into(),
            created_at: 0,
            image_sizes: Default::default(),
            nodes: vec![
                tree_node("root", None, &["root-0", "root-1"], 0),
                tree_node("take-a", Some("root"), &["a-0", "a-1"], 1),
                tree_node("take-b", Some("root"), &["b-0", "b-1"], 2),
                tree_node("empty-child", Some("take-a"), &[], 3),
                tree_node("descendant", Some("empty-child"), &["d-0", "d-1"], 4),
                tree_node("other-root", None, &["o-0"], 5),
                tree_node("single-c", Some("root"), &["c-0"], 6),
                tree_node("single-d", Some("root"), &["d-single"], 7),
                tree_node("another-root", None, &["r-0"], 8),
            ],
        };
        let memory = LightboxMemory::default();
        let a0 = LightboxLocation {
            node_id: "take-a".into(),
            image: "a-0".into(),
        };
        let a1 = lightbox_target(&board, &memory, &a0, 1, 0).expect("next image");
        assert_eq!(a1.node_id, "take-a");
        assert_eq!(a1.image, "a-1");

        // Multi-image cards are self-contained: no hop out at the edge.
        assert_eq!(lightbox_target(&board, &memory, &a1, 1, 0), None);
        assert_eq!(lightbox_target(&board, &memory, &a0, -1, 0), None);

        // Single-image siblings hop between each other, skipping multi-image cards.
        let c0 = LightboxLocation {
            node_id: "single-c".into(),
            image: "c-0".into(),
        };
        let d = lightbox_target(&board, &memory, &c0, 1, 0).expect("next single sibling");
        assert_eq!(d.node_id, "single-d");
        assert_eq!(
            lightbox_target(&board, &memory, &d, -1, 0),
            Some(c0.clone())
        );
        assert_eq!(lightbox_target(&board, &memory, &c0, -1, 0), None);

        // Vertical moves land on the first image of a multi-image card.
        let parent = lightbox_target(&board, &memory, &a0, 0, -1).expect("parent image");
        assert_eq!(parent.node_id, "root");
        assert_eq!(parent.image, "root-0");
        let descendant = lightbox_target(&board, &memory, &a0, 0, 1).expect("descendant image");
        assert_eq!(descendant.node_id, "descendant");
        assert_eq!(descendant.image, "d-0");

        // Top-level single-image cards are siblings of each other.
        let root_1 = lightbox_target(&board, &memory, &parent, 1, 0).expect("next root image");
        assert_eq!(root_1.image, "root-1");
        assert_eq!(lightbox_target(&board, &memory, &root_1, 1, 0), None);
        let other_root = LightboxLocation {
            node_id: "other-root".into(),
            image: "o-0".into(),
        };
        let another_root =
            lightbox_target(&board, &memory, &other_root, 1, 0).expect("next root card");
        assert_eq!(another_root.node_id, "another-root");
        assert_eq!(
            lightbox_target(&board, &memory, &another_root, -1, 0),
            Some(other_root)
        );
    }

    #[test]
    fn reference_images_are_browsed_apart_from_the_outputs() {
        let mut node = tree_node("node", None, &["out-0"], 0);
        node.attachments = vec!["ref-0".into(), "ref-1".into()];
        let board = Board {
            id: "board".into(),
            title: "Board".into(),
            created_at: 0,
            image_sizes: Default::default(),
            nodes: vec![node, tree_node("sibling", None, &["s-0"], 1)],
        };
        let memory = LightboxMemory::default();
        let at = |image: &str| LightboxLocation {
            node_id: "node".into(),
            image: image.into(),
        };

        assert_eq!(
            lightbox_target(&board, &memory, &at("ref-0"), 1, 0),
            Some(at("ref-1"))
        );
        // The last reference neither wraps into the outputs nor hops cards.
        assert_eq!(lightbox_target(&board, &memory, &at("ref-1"), 1, 0), None);
        assert_eq!(
            lightbox_target(&board, &memory, &at("out-0"), 1, 0).map(|target| target.node_id),
            Some("sibling".into())
        );
    }

    #[test]
    fn vertical_navigation_returns_to_the_remembered_image_and_card() {
        let board = Board {
            id: "board".into(),
            title: "Board".into(),
            created_at: 0,
            image_sizes: Default::default(),
            nodes: vec![
                tree_node("root", None, &["root-0", "root-1"], 0),
                tree_node("take-a", Some("root"), &["a-0", "a-1"], 1),
                tree_node("take-b", Some("root"), &["b-0", "b-1"], 2),
            ],
        };
        let mut memory = LightboxMemory::default();
        let b1 = LightboxLocation {
            node_id: "take-b".into(),
            image: "b-1".into(),
        };
        let root_1 = LightboxLocation {
            node_id: "root".into(),
            image: "root-1".into(),
        };
        memory.remember(&b1);
        memory.remember(&root_1);
        memory.descent.insert("root".into(), "take-b".into());

        // Up lands on the parent's last shown image, not its first.
        assert_eq!(
            lightbox_target(&board, &memory, &b1, 0, -1),
            Some(root_1.clone())
        );
        // Down returns to the card and image the user came up from, even
        // though take-a is the first child.
        assert_eq!(
            lightbox_target(&board, &memory, &root_1, 0, 1),
            Some(b1.clone())
        );

        // A stale descent entry falls back to the first child.
        memory.descent.insert("root".into(), "gone".into());
        let fallback = lightbox_target(&board, &memory, &root_1, 0, 1).expect("first child");
        assert_eq!(fallback.node_id, "take-a");
        assert_eq!(fallback.image, "a-0");
    }
}
