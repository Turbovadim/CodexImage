//! The full-screen image viewer: its zoom and pan state, keyboard navigation
//! between the images of a board, and the toolbar layered over the image.

use super::app::AppView;
use super::app::Overlay;
use super::canvas_view::DragState;
use super::composer::control_button;
use super::format::image_format_for_path;
use super::input::TextInputMode;
use super::keymap::{Generate, LightboxDown, LightboxLeft, LightboxRight, LightboxUp};
use super::theme;
use crate::model::{Board, BoardNode, NewNodesRequest};
use gpui::{
    AnyElement, ClipboardItem, Context, Focusable, FontWeight, Image, ImgResourceLoader,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, PinchEvent, Pixels,
    Point, Resource, Role, ScrollWheelEvent, StyledImage, Window, div, img, prelude::*, px,
};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) const LIGHTBOX_MIN_ZOOM: f32 = 1.;

pub(super) const LIGHTBOX_MAX_ZOOM: f32 = 8.;

pub(super) struct Lightbox {
    pub(super) node_id: String,
    pub(super) image: String,
    pub(super) zoom: f32,
    pub(super) pan_x: f32,
    pub(super) pan_y: f32,
    pub(super) pending: Option<LightboxLocation>,
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
        if self.pending.as_ref() != Some(expected) {
            return false;
        }
        let target = self
            .pending
            .take()
            .expect("pending target was just checked");
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
    current: &LightboxLocation,
    horizontal: i32,
    vertical: i32,
) -> Option<LightboxLocation> {
    if horizontal == 0 && vertical == 0 {
        return None;
    }
    let node = board.nodes.iter().find(|node| node.id == current.node_id)?;
    if horizontal != 0 {
        let index = node
            .images
            .iter()
            .position(|image| image == &current.image)
            .unwrap_or(0) as i32
            + horizontal;
        if index >= 0 && (index as usize) < node.images.len() {
            return Some(LightboxLocation {
                node_id: node.id.clone(),
                image: node.images[index as usize].clone(),
            });
        }
        let parent_id = node.parent_id.as_ref()?;
        let mut siblings: Vec<_> = board
            .nodes
            .iter()
            .filter(|candidate| {
                candidate.parent_id.as_ref() == Some(parent_id) && !candidate.images.is_empty()
            })
            .collect();
        siblings.sort_by_key(|candidate| (candidate.created_at, &candidate.id));
        let current_index = siblings
            .iter()
            .position(|candidate| candidate.id == node.id)?;
        let sibling_index = current_index as i32 + horizontal;
        if sibling_index < 0 || (sibling_index as usize) >= siblings.len() {
            return None;
        }
        let sibling = siblings[sibling_index as usize];
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
            if let Some(image) = parent.images.last() {
                return Some(LightboxLocation {
                    node_id: parent.id.clone(),
                    image: image.clone(),
                });
            }
            parent_id = parent.parent_id.as_deref();
        }
        return None;
    }

    let mut queue: VecDeque<&BoardNode> = board
        .nodes
        .iter()
        .filter(|candidate| candidate.parent_id.as_deref() == Some(&node.id))
        .collect();
    while let Some(child) = queue.pop_front() {
        if let Some(image) = child.images.last() {
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

impl AppView {
    pub(super) fn navigate_left(
        &mut self,
        _: &LightboxLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.lightbox_input_focused(window, cx) {
            self.navigate_lightbox(-1, 0, cx)
        }
    }

    pub(super) fn navigate_right(
        &mut self,
        _: &LightboxRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.lightbox_input_focused(window, cx) {
            self.navigate_lightbox(1, 0, cx)
        }
    }

    pub(super) fn navigate_up(
        &mut self,
        _: &LightboxUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.lightbox_input_focused(window, cx) {
            self.navigate_lightbox(0, -1, cx)
        }
    }

    pub(super) fn navigate_down(
        &mut self,
        _: &LightboxDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.lightbox_input_focused(window, cx) {
            self.navigate_lightbox(0, 1, cx)
        }
    }

    fn lightbox_input_focused(&self, window: &Window, cx: &Context<Self>) -> bool {
        matches!(self.overlay, Overlay::Lightbox(_))
            && self.modal_input.focus_handle(cx).is_focused(window)
    }

    pub(super) fn open_lightbox(
        &mut self,
        node_id: String,
        image: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal_input.update(cx, |input, cx| {
            input.set_mode(TextInputMode::SingleLine, cx);
            input.set_placeholder("Refine this image…", cx);
            input.clear(cx);
        });
        self.overlay = Overlay::Lightbox(Lightbox {
            node_id,
            image,
            zoom: LIGHTBOX_MIN_ZOOM,
            pan_x: 0.,
            pan_y: 0.,
            pending: None,
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
        let Some(target) = lightbox_target(board, &origin, horizontal, vertical) else {
            return;
        };
        let Overlay::Lightbox(current) = &mut self.overlay else {
            return;
        };
        current.request(target);
        self.modal_input.update(cx, |input, cx| input.clear(cx));
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
            match window.use_asset::<ImgResourceLoader>(&resource, cx) {
                Some(Ok(image)) if image.frame_count() > 0 => {
                    if let Overlay::Lightbox(lightbox) = &mut self.overlay {
                        lightbox.commit_pending(&pending);
                    }
                }
                Some(Ok(_)) => load_error = Some("Image contains no displayable frames".to_owned()),
                Some(Err(error)) => load_error = Some(format!("Could not load image: {error}")),
                None => {}
            }
        }
        if let Some(error) = load_error {
            if let Overlay::Lightbox(lightbox) = &mut self.overlay {
                lightbox.pending = None;
            }
            self.show_error(error, cx);
        }

        let Some((board, current)) = self.board.as_ref().and_then(|board| {
            let Overlay::Lightbox(lightbox) = &self.overlay else {
                return None;
            };
            Some((board, lightbox.displayed_location()))
        }) else {
            return;
        };
        let current_resource =
            Resource::Path(Arc::from(self.display_image_path(&current.image, true)));
        let _ = window.use_asset::<ImgResourceLoader>(&current_resource, cx);

        let mut previous_path = None;
        for horizontal in [-1, 1] {
            let Some(target) = lightbox_target(board, &current, horizontal, 0) else {
                continue;
            };
            let path = self.display_image_path(&target.image, true);
            if previous_path.as_ref() == Some(&path) {
                continue;
            }
            previous_path = Some(path.clone());
            let resource = Resource::Path(Arc::from(path));
            let _ = window.use_asset::<ImgResourceLoader>(&resource, cx);
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
        let viewport = window.viewport_size();
        if let Overlay::Lightbox(lightbox) = &mut self.overlay {
            lightbox.zoom_at(
                factor,
                focal,
                f32::from(viewport.width),
                f32::from(viewport.height),
                image_ratio,
            );
            cx.notify();
        }
    }

    fn lightbox_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(DragState::Lightbox { start, origin }) = &self.drag else {
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
        let viewport = window.viewport_size();
        if let Overlay::Lightbox(lightbox) = &mut self.overlay {
            lightbox.pan_to(
                origin.0 + f32::from(event.position.x - start.x),
                origin.1 + f32::from(event.position.y - start.y),
                f32::from(viewport.width),
                f32::from(viewport.height),
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

    pub(super) fn continue_from_lightbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Overlay::Lightbox(lightbox) = &self.overlay else {
            return;
        };
        let (node_id, image) = (lightbox.node_id.clone(), lightbox.image.clone());
        let prompt = self.modal_input.read(cx).content().trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        let request = NewNodesRequest {
            prompt,
            parent_id: Some(node_id.clone()),
            source_images: Some(vec![image]),
            aspect: self
                .node(&node_id)
                .map(|node| node.aspect)
                .unwrap_or_else(|| "auto".into()),
            count: 1,
            attachment_paths: Vec::new(),
            attachment_urls: Vec::new(),
        };
        match self
            .board_id()
            .map(str::to_owned)
            .and_then(|board_id| self.engine.add_and_start(&board_id, request))
        {
            Ok(_) => {
                self.modal_input.update(cx, |input, cx| input.clear(cx));
                self.overlay = Overlay::None;
                window.focus(&self.focus, cx);
            }
            Err(error) => self.show_error(error, cx),
        }
        cx.notify();
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
        let display_image = match window.use_asset::<ImgResourceLoader>(&resource, cx) {
            Some(Ok(image)) if image.frame_count() > 0 => img(image),
            _ => img(thumbnail_path),
        };
        let node = self.node(&lightbox.node_id);
        let image_index = node
            .as_ref()
            .and_then(|node| {
                node.images
                    .iter()
                    .position(|image| image == &lightbox.image)
            })
            .unwrap_or(0);
        let total = node.as_ref().map(|node| node.images.len()).unwrap_or(1);
        let copy_path = path.clone();
        let save_path = path.clone();
        let open_path = path.clone();
        let branch_node = lightbox.node_id.clone();
        let branch_image = lightbox.image.clone();
        let locate_node = lightbox.node_id.clone();
        let viewport_width = f32::from(window.viewport_size().width);
        let viewport_height = f32::from(window.viewport_size().height);
        let image_ratio = normalized_image_ratio(
            self.image_ratios
                .get(&lightbox.image)
                .copied()
                .unwrap_or(1.),
        );
        let (fit_width, fit_height) =
            fitted_image_size(viewport_width, viewport_height, image_ratio);
        let image_width = fit_width * lightbox.zoom;
        let image_height = fit_height * lightbox.zoom;
        let (pan_x, pan_y) = lightbox.clamped_pan(viewport_width, viewport_height, image_ratio);
        let image_left = (viewport_width - image_width) / 2. + pan_x;
        let image_top = (viewport_height - image_height) / 2. + pan_y;
        let lightbox_dragging = matches!(&self.drag, Some(DragState::Lightbox { .. }));
        let stage_cursor = if lightbox_dragging {
            gpui::CursorStyle::ClosedHand
        } else if lightbox.zoom > LIGHTBOX_MIN_ZOOM {
            gpui::CursorStyle::OpenHand
        } else {
            gpui::CursorStyle::Arrow
        };
        let continue_width = (viewport_width - 48.).clamp(240., 540.);
        let toolbar = div()
            .absolute()
            .top(px(16.))
            .right(px(16.))
            .flex()
            .gap_2()
            .occlude()
            .child(control_button(
                "Branch (B)",
                cx.listener(move |this, _, window, cx| {
                    this.branch_node(&branch_node, Some(branch_image.clone()), window, cx);
                    this.overlay = Overlay::None;
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
                    let _ = std::process::Command::new("open").arg(&open_path).spawn();
                }),
            ))
            .child(control_button(
                "×",
                cx.listener(|this, _, window, cx| {
                    this.close_overlay(window, cx);
                    cx.notify();
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
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
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
                        if matches!(&this.overlay, Overlay::Lightbox(lightbox) if lightbox.zoom <= 1.)
                        {
                            this.close_overlay(window, cx);
                            cx.notify();
                        }
                    }))
                    .child(
                        display_image
                            .absolute()
                            .left(px(image_left))
                            .top(px(image_top))
                            .w(px(image_width))
                            .h(px(image_height))
                            .object_fit(ObjectFit::Contain),
                    ),
            )
            .child(toolbar)
            .child(
                div()
                    .absolute()
                    .left(px((viewport_width - continue_width) / 2.))
                    .bottom(px(20.))
                    .w(px(continue_width))
                    .rounded_xl()
                    .border_1()
                    .border_color(theme::line())
                    .bg(theme::raised().opacity(0.94))
                    .occlude()
                    .p_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .px_3()
                            .child(self.modal_input.clone()),
                    )
                    .child(
                        div()
                            .id("quick-continue")
                            .rounded_lg()
                            .bg(theme::accent_strong())
                            .px_4()
                            .py_2()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(gpui::white())
                            .cursor_pointer()
                            .child("Continue ↵")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.generate(&Generate, window, cx)
                            })),
                    )
            );
        if total > 1 {
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
                    .child(format!("{} / {}", image_index + 1, total)),
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

    fn copy_image(&mut self, path: &Path, cx: &mut Context<Self>) {
        match fs::read(path).and_then(|bytes| {
            let format = image_format_for_path(path)
                .ok_or_else(|| std::io::Error::other("unsupported image format"))?;
            cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(format, bytes)));
            Ok(())
        }) {
            Ok(()) => self.show_toast("Image copied".into(), false, None, cx),
            Err(error) => self.show_error(error, cx),
        }
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
    use super::{Lightbox, LightboxLocation, lightbox_target};
    use crate::model::{Board, BoardNode, NodeStatus};
    use gpui::{point, px};

    fn tree_node(id: &str, parent_id: Option<&str>, images: &[&str], created_at: i64) -> BoardNode {
        BoardNode {
            id: id.into(),
            parent_id: parent_id.map(str::to_owned),
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
        }
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
            nodes: vec![
                tree_node("root", None, &["root-0"], 0),
                tree_node("take-a", Some("root"), &["a-0", "a-1"], 1),
                tree_node("take-b", Some("root"), &["b-0", "b-1"], 2),
                tree_node("empty-child", Some("take-a"), &[], 3),
                tree_node("descendant", Some("empty-child"), &["d-0"], 4),
            ],
        };
        let a0 = LightboxLocation {
            node_id: "take-a".into(),
            image: "a-0".into(),
        };
        let a1 = lightbox_target(&board, &a0, 1, 0).expect("next image");
        assert_eq!(a1.node_id, "take-a");
        assert_eq!(a1.image, "a-1");

        let b0 = lightbox_target(&board, &a1, 1, 0).expect("next sibling");
        assert_eq!(b0.node_id, "take-b");
        assert_eq!(b0.image, "b-0");
        assert_eq!(lightbox_target(&board, &b0, -1, 0), Some(a1.clone()));

        let parent = lightbox_target(&board, &a0, 0, -1).expect("parent image");
        assert_eq!(parent.node_id, "root");
        assert_eq!(parent.image, "root-0");
        let descendant = lightbox_target(&board, &a0, 0, 1).expect("descendant image");
        assert_eq!(descendant.node_id, "descendant");
        assert_eq!(descendant.image, "d-0");
    }
}
