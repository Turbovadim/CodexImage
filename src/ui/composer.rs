//! The prompt composer with its attachments, plus the header, gallery button,
//! and the empty-board welcome screen.

use super::app::AppView;
use super::app::Overlay;
use super::input::TextInputEvent;
use super::keymap::{AddAttachment, FocusPrompt, Generate, OpenBoards, ToggleGallery};
use super::theme;
use super::tooltip::{tip, tip_with_shortcut};
use crate::APP_NAME;
use crate::model::NewNodesRequest;
use anyhow::Result;
use gpui::{
    AnyElement, App, Context, Focusable, FontWeight, Image, ImageFormat, ObjectFit,
    PathPromptOptions, Role, SharedString, StyledImage, Window, div, img, prelude::*, px,
};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

pub(super) const ASPECTS: &[&str] = &["auto", "1:1", "16:9", "9:16", "4:3", "3:4"];

pub(super) const SAMPLES: &[&str] = &[
    "A cozy cabin in a snowy forest at dusk, warm light in the windows",
    "Isometric illustration of a tiny home office, pastel palette",
    "Logo concept for a coffee brand called \"Ember\", minimal, flat",
    "Studio photo of a perfume bottle on black marble, dramatic lighting",
];

#[derive(Clone)]
pub(super) struct ComposerTarget {
    pub(super) node_id: String,
    pub(super) prompt: String,
    pub(super) source_image: Option<String>,
}

pub(super) fn control_button(
    label: impl Into<SharedString>,
    listener: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let label = label.into();
    div()
        .id(label.clone())
        .role(Role::Button)
        .aria_label(label.clone())
        .rounded_lg()
        .border_1()
        .border_color(theme::line())
        .bg(theme::raised())
        .px_3()
        .py_2()
        .text_sm()
        .text_color(theme::dim())
        .cursor_pointer()
        .hover(|style| style.border_color(theme::faint()).text_color(theme::ink()))
        .child(label)
        .on_click(listener)
        .into_any_element()
}

impl AppView {
    pub(super) fn handle_input_event(&mut self, event: &TextInputEvent, cx: &mut Context<Self>) {
        match event {
            TextInputEvent::PastedImages(images) => {
                for image in images {
                    if let Ok(path) = self.save_pending_clipboard_image(image) {
                        self.attachments.push(path);
                    }
                }
            }
            TextInputEvent::PastedPaths(paths) => self.queue_attachments(paths.clone()),
        }
        cx.notify();
    }

    fn save_pending_clipboard_image(&self, image: &Image) -> Result<PathBuf> {
        let extension = match image.format {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Webp => "webp",
            ImageFormat::Gif => "gif",
            ImageFormat::Svg => "svg",
            ImageFormat::Bmp => "bmp",
            ImageFormat::Tiff => "tiff",
            ImageFormat::Ico => "ico",
            ImageFormat::Pnm => "pnm",
        };
        let directory = self
            .engine
            .repository()
            .paths()
            .root
            .join("pending-attachments");
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("clipboard-{}.{}", Uuid::new_v4(), extension));
        fs::write(&path, &image.bytes)?;
        Ok(path)
    }

    pub(super) fn queue_attachments(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            if self.attachments.len() >= crate::model::MAX_ATTACHMENTS {
                break;
            }
            if path.is_file()
                && image::ImageFormat::from_path(&path).is_ok()
                && !self.attachments.contains(&path)
            {
                self.attachments.push(path);
            }
        }
    }

    pub(super) fn add_attachment(
        &mut self,
        _: &AddAttachment,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach images".into()),
        });
        cx.spawn(async move |weak, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let _ = weak.update(cx, |view, cx| {
                view.queue_attachments(paths);
                cx.notify();
            });
        })
        .detach();
    }

    /// Drops the attachment list, deleting the copies made for pasted images.
    fn discard_pending_attachments(&mut self) {
        let pending = self
            .engine
            .repository()
            .paths()
            .root
            .join("pending-attachments");
        for path in self.attachments.drain(..) {
            if path.starts_with(&pending) {
                let _ = fs::remove_file(path);
            }
        }
    }

    pub(super) fn generate_from_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.prompt.read(cx).content().trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        let board_id = match self.ensure_board() {
            Ok(id) => id,
            Err(error) => {
                self.show_error(error, cx);
                return;
            }
        };
        let target = self.target.take();
        let request = NewNodesRequest {
            prompt,
            parent_id: target.as_ref().map(|target| target.node_id.clone()),
            source_images: target
                .as_ref()
                .and_then(|target| target.source_image.clone().map(|image| vec![image])),
            aspect: ASPECTS[self.aspect_index].to_owned(),
            count: self.count,
            attachment_paths: self.attachments.clone(),
            attachment_urls: Vec::new(),
        };
        match self.engine.add_and_start(&board_id, request) {
            Ok(_) => {
                self.discard_pending_attachments();
                self.prompt.update(cx, |input, cx| input.clear(cx));
                window.focus(&self.prompt.focus_handle(cx), cx);
            }
            Err(error) => {
                self.target = target;
                self.show_error(error, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn focus_prompt(
        &mut self,
        _: &FocusPrompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.overlay, Overlay::None) {
            window.focus(&self.prompt.focus_handle(cx), cx);
        }
    }

    pub(super) fn render_composer(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let width = 660_f32.min(f32::from(window.viewport_size().width) - 40.);
        let left = (f32::from(window.viewport_size().width) - width) / 2.;
        let mut composer = div()
            .absolute()
            .left(px(left))
            .bottom(px(24.))
            .w(px(width))
            .rounded(px(16.))
            .border_1()
            .border_color(theme::line())
            .bg(theme::raised().opacity(0.97))
            .p_3()
            .occlude();
        if let Some(target) = &self.target {
            let cancel_id = target.node_id.clone();
            composer = composer.child(
                div()
                    .mb_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(theme::dim())
                    .child(div().text_color(theme::accent()).child("Branching from"))
                    .child(target.prompt.chars().take(70).collect::<String>())
                    .child(div().flex_1())
                    .child(
                        div()
                            .id(SharedString::from(format!("cancel-target-{cancel_id}")))
                            .cursor_pointer()
                            .text_color(theme::faint())
                            .child("×")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.target = None;
                                cx.notify();
                            })),
                    ),
            );
        }
        if !self.attachments.is_empty() {
            let mut strip = div().mb_2().flex().gap_2();
            for (index, path) in self.attachments.iter().enumerate() {
                strip = strip.child(
                    div()
                        .relative()
                        .child(
                            img(path.clone())
                                .size(px(42.))
                                .object_fit(ObjectFit::Cover)
                                .rounded_md(),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("remove-attachment-{index}")))
                                .role(Role::Button)
                                .aria_label("Remove attachment")
                                .absolute()
                                .top(px(-5.))
                                .right(px(-5.))
                                .size(px(16.))
                                .rounded_full()
                                .border_1()
                                .border_color(theme::line())
                                .bg(theme::background())
                                .text_center()
                                .text_xs()
                                .text_color(theme::dim())
                                .cursor_pointer()
                                .hover(|style| {
                                    style
                                        .bg(theme::danger().opacity(0.2))
                                        .text_color(theme::danger())
                                })
                                .tooltip(tip("Remove attachment"))
                                .child("×")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if index < this.attachments.len() {
                                        this.attachments.remove(index);
                                    }
                                    cx.notify();
                                })),
                        ),
                );
            }
            composer = composer.child(strip);
        }
        let pill = |id: &'static str, label: String| {
            div()
                .id(id)
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(theme::line())
                .text_xs()
                .text_color(theme::dim())
                .cursor_pointer()
                .hover(|style| style.border_color(theme::faint()).text_color(theme::ink()))
                .child(label)
        };
        let ready = !self.prompt.read(cx).content().trim().is_empty();
        let attachments_full = self.attachments.len() >= crate::model::MAX_ATTACHMENTS;
        composer = composer.child(
            div()
                .flex()
                .items_end()
                .gap_2()
                .child(
                    pill("aspect", ASPECTS[self.aspect_index].to_owned())
                        .tooltip(tip("Aspect ratio — click to cycle"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.aspect_index = (this.aspect_index + 1) % ASPECTS.len();
                            cx.notify();
                        })),
                )
                .child(
                    pill("count", format!("×{}", self.count))
                        .tooltip(tip("Images per run — click to cycle 1–4"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.count = this.count % 4 + 1;
                            cx.notify();
                        })),
                )
                .child(
                    pill("attach", "Attach".into())
                        .when(attachments_full, |pill| {
                            pill.text_color(theme::faint())
                                .border_color(theme::line().opacity(0.5))
                        })
                        .tooltip(tip_with_shortcut(
                            if attachments_full {
                                "Attachment limit reached"
                            } else {
                                "Attach reference images"
                            },
                            Some("⌘O"),
                        ))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.add_attachment(&AddAttachment, window, cx)
                        })),
                )
                .child(div().flex_1().child(self.prompt.clone()))
                .child(
                    div()
                        .id("send")
                        .role(Role::Button)
                        .aria_label("Generate")
                        .size(px(28.))
                        .rounded_lg()
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(ready, |send| {
                            send.bg(theme::accent_strong().opacity(0.18))
                                .text_color(theme::accent())
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::accent_strong().opacity(0.32)))
                        })
                        .when(!ready, |send| {
                            send.bg(theme::hover()).text_color(theme::faint())
                        })
                        .tooltip(tip_with_shortcut(
                            if ready {
                                "Generate · ⇧↵ for a new line"
                            } else {
                                "Describe an image first"
                            },
                            Some("↵"),
                        ))
                        .child("↑")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.generate(&Generate, window, cx)),
                        ),
                ),
        );
        composer.into_any_element()
    }

    pub(super) fn render_empty(&self, _window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let mut samples = div()
            .mt_4()
            .flex()
            .flex_wrap()
            .justify_center()
            .gap_2()
            .w(px(600.));
        for (index, sample) in SAMPLES.iter().enumerate() {
            let text = (*sample).to_owned();
            samples = samples.child(
                div()
                    .id(SharedString::from(format!("sample-{index}")))
                    .rounded_full()
                    .border_1()
                    .border_color(theme::line())
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(theme::dim())
                    .cursor_pointer()
                    .hover(|style| {
                        style
                            .border_color(theme::accent().opacity(0.6))
                            .text_color(theme::ink())
                    })
                    .child(*sample)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.prompt
                            .update(cx, |input, cx| input.set_content(text.clone(), cx));
                        window.focus(&this.prompt.focus_handle(cx), cx);
                    })),
            );
        }
        div().absolute().inset_0().pb(px(170.)).flex().flex_col().items_center().justify_center()
            .child(div().text_size(px(42.)).text_color(theme::accent()).child("❖"))
            .child(div().mt_2().text_size(px(26.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::ink()).child("What should we create?"))
            .child(div().mt_2().w(px(600.)).text_center().text_sm().text_color(theme::dim()).child("Ask for one image or a complete ordered series. Use ×N for parallel takes, then branch, continue, and regenerate on an infinite canvas."))
            .child(samples)
            .child(div().mt_5().text_xs().text_color(theme::faint()).child("/ prompt   ⌘K boards   G gallery   F fit view   ⌘0 actual size   Esc cancel"))
            .into_any_element()
    }

    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let title = self
            .board
            .as_ref()
            .map(|board| board.title.as_str())
            .unwrap_or(APP_NAME);
        let generating = self.engine.active_count() > 0;
        let mut header = div()
            .id("board-switcher")
            .absolute()
            .top(px(14.))
            .left(px(18.))
            .flex()
            .items_center()
            .gap_2()
            .rounded_xl()
            .border_1()
            .border_color(theme::line())
            .bg(theme::raised().opacity(0.96))
            .px_3()
            .py_2()
            .cursor_pointer()
            .occlude()
            .hover(|style| style.border_color(theme::faint()).bg(theme::hover()))
            .tooltip(tip_with_shortcut("Switch board", Some("⌘K")))
            .on_click(cx.listener(|this, _, window, cx| this.open_boards(&OpenBoards, window, cx)))
            .child(div().text_color(theme::accent()).child("❖"))
            .child(
                div()
                    .max_w(px(230.))
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::ink())
                    .child(title.to_owned()),
            );
        if generating {
            header = header.child(div().size(px(8.)).rounded_full().bg(theme::accent()));
        }
        header
            .child(div().text_xs().text_color(theme::faint()).child("⌄"))
            .into_any_element()
    }

    pub(super) fn render_gallery_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.board
            .as_ref()?
            .nodes
            .iter()
            .any(|node| !node.images.is_empty())
            .then(|| {
                div()
                    .id("gallery-button")
                    .absolute()
                    .top(px(14.))
                    .right(px(18.))
                    .rounded_xl()
                    .border_1()
                    .border_color(theme::line())
                    .bg(theme::raised().opacity(0.96))
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(theme::dim())
                    .cursor_pointer()
                    .occlude()
                    .hover(|style| {
                        style
                            .border_color(theme::faint())
                            .bg(theme::hover())
                            .text_color(theme::ink())
                    })
                    .tooltip(tip_with_shortcut(
                        "Browse every image on this board",
                        Some("G"),
                    ))
                    .child("▦  Gallery")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_gallery(&ToggleGallery, window, cx)
                    }))
                    .into_any_element()
            })
    }
}
