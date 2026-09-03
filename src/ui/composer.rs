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
use anyhow::{Context as _, Result};
use gpui::{
    AnyElement, App, Context, Focusable, FontWeight, Image, ImageFormat, ObjectFit,
    PathPromptOptions, Role, SharedString, StyledImage, Window, div, img, prelude::*, px,
};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

pub(super) const ASPECTS: &[&str] = &["auto", "1:1", "16:9", "9:16", "4:3", "3:4"];

pub(super) const SAMPLES: &[&str] = &[
    "A cozy cabin in a snowy forest at dusk, warm light in the windows",
    "Isometric illustration of a tiny home office, pastel palette",
    "Logo concept for a coffee brand called \"Ember\", minimal, flat",
    "Studio photo of a perfume bottle on black marble, dramatic lighting",
];

/// Persists clipboard image bytes without blocking GPUI's application thread.
/// On a partial failure, removes every temporary file from this paste batch.
fn save_pending_clipboard_images(
    directory: PathBuf,
    images: Arc<[Image]>,
    save_count: usize,
) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(&directory).context("creating the pending attachment directory")?;
    let mut paths = Vec::with_capacity(save_count);
    for image in images.iter().take(save_count) {
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
        let path = directory.join(format!("clipboard-{}.{}", Uuid::new_v4(), extension));
        if let Err(error) = fs::write(&path, &image.bytes) {
            let _ = fs::remove_file(&path);
            for saved in paths {
                let _ = fs::remove_file(saved);
            }
            return Err(error).context("saving a pasted image");
        }
        paths.push(path);
    }
    Ok(paths)
}

#[derive(Clone, Eq, PartialEq)]
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
                let available = crate::model::MAX_ATTACHMENTS
                    .saturating_sub(self.attachments.len() + self.pending_attachment_writes);
                let save_count = available.min(images.len());
                if save_count == 0 {
                    self.show_toast(
                        format!(
                            "Attachment limit reached ({})",
                            crate::model::MAX_ATTACHMENTS
                        ),
                        false,
                        None,
                        cx,
                    );
                    return;
                }
                if save_count < images.len() {
                    self.show_toast(
                        format!(
                            "Saving {save_count} images; attachment limit is {}",
                            crate::model::MAX_ATTACHMENTS
                        ),
                        false,
                        None,
                        cx,
                    );
                }
                self.pending_attachment_writes += save_count;
                let directory = self
                    .engine
                    .repository()
                    .paths()
                    .root
                    .join("pending-attachments");
                let images = Arc::clone(images);
                cx.spawn(async move |weak, cx| {
                    let result = smol::unblock(move || {
                        save_pending_clipboard_images(directory, images, save_count)
                    })
                    .await;
                    let Some(view) = weak.upgrade() else {
                        // The app closed after the batch reached disk. Those
                        // files never became owned attachments, so remove them.
                        if let Ok(paths) = result {
                            smol::unblock(move || {
                                for path in paths {
                                    let _ = fs::remove_file(path);
                                }
                            })
                            .await;
                        }
                        return;
                    };
                    view.update(cx, |view, cx| {
                        view.pending_attachment_writes =
                            view.pending_attachment_writes.saturating_sub(save_count);
                        match result {
                            // These paths already own reserved slots and came
                            // from GPUI-decoded clipboard images.
                            Ok(paths) => view.attachments.extend(paths),
                            Err(error) => view.show_error(error, cx),
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            TextInputEvent::PastedPaths(paths) => self.queue_attachments(paths.clone()),
        }
        cx.notify();
    }

    pub(super) fn queue_attachments(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            if self.attachments.len() + self.pending_attachment_writes
                >= crate::model::MAX_ATTACHMENTS
            {
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
        if self.attachments.len() + self.pending_attachment_writes >= crate::model::MAX_ATTACHMENTS
        {
            self.show_toast(
                format!(
                    "Attachment limit reached ({})",
                    crate::model::MAX_ATTACHMENTS
                ),
                false,
                None,
                cx,
            );
            return;
        }
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

    fn remove_attachment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.attachments.len() {
            return;
        }
        let path = self.attachments.remove(index);
        let pending = self
            .engine
            .repository()
            .paths()
            .root
            .join("pending-attachments");
        if path.starts_with(pending) {
            cx.background_spawn(async move {
                let _ = fs::remove_file(path);
            })
            .detach();
        }
        cx.notify();
    }

    /// Removes the attachments consumed by one successful submission while
    /// preserving any references the user added as it was starting.
    fn discard_submitted_attachments(&mut self, submitted: &[PathBuf], cx: &mut Context<Self>) {
        let pending = self
            .engine
            .repository()
            .paths()
            .root
            .join("pending-attachments");
        let mut paths = Vec::new();
        self.attachments.retain(|path| {
            if !submitted.contains(path) {
                return true;
            }
            if path.starts_with(&pending) {
                paths.push(path.clone());
            }
            false
        });
        if !paths.is_empty() {
            cx.background_spawn(async move {
                for path in paths {
                    let _ = fs::remove_file(path);
                }
            })
            .detach();
        }
    }

    pub(super) fn generate_from_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_submission_pending {
            self.show_toast("A generation is already starting".into(), false, None, cx);
            return;
        }
        if self.pending_attachment_writes > 0 {
            self.show_toast(
                "Wait for pasted attachments to finish saving".into(),
                false,
                None,
                cx,
            );
            return;
        }
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
        let target = self.target.clone();
        let submitted_prompt = prompt.clone();
        let submitted_attachments = self.attachments.clone();
        let request = NewNodesRequest {
            prompt,
            parent_id: target.as_ref().map(|target| target.node_id.clone()),
            source_images: target
                .as_ref()
                .and_then(|target| target.source_image.clone().map(|image| vec![image])),
            aspect: ASPECTS[self.aspect_index].to_owned(),
            count: self.count,
            attachment_paths: submitted_attachments.clone(),
            attachment_urls: Vec::new(),
            position: None,
        };
        self.composer_submission_pending = true;
        cx.notify();
        let engine = self.engine.clone();
        let submission = cx
            .background_spawn(async move { engine.add_and_start(&board_id, request).map(|_| ()) });
        cx.spawn_in(window, async move |weak, cx| {
            let result = submission.await;
            let _ = weak.update_in(cx, |view, window, cx| {
                view.composer_submission_pending = false;
                match result {
                    Ok(()) => {
                        view.discard_submitted_attachments(&submitted_attachments, cx);
                        if view.prompt.read(cx).content().trim() == submitted_prompt {
                            view.prompt.update(cx, |input, cx| input.clear(cx));
                        }
                        if view.target == target {
                            view.target = None;
                        }
                        window.focus(&view.prompt.focus_handle(cx), cx);
                    }
                    Err(error) => view.show_error(error, cx),
                }
                cx.notify();
            });
        })
        .detach();
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
        let submission_pending = self.composer_submission_pending;
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
                            .role(Role::Button)
                            .aria_label("Cancel branching target")
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
                                .when(submission_pending, |remove| {
                                    remove.text_color(theme::faint()).cursor_default()
                                })
                                .when(!submission_pending, |remove| {
                                    remove.on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_attachment(index, cx);
                                    }))
                                }),
                        ),
                );
            }
            composer = composer.child(strip);
        }
        let pill = |id: &'static str, label: String| {
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(label.clone())
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
        let ready = !submission_pending
            && self.pending_attachment_writes == 0
            && !self.prompt.read(cx).content().trim().is_empty();
        let attachments_full = self.attachments.len() + self.pending_attachment_writes
            >= crate::model::MAX_ATTACHMENTS;
        let attach_label = if self.pending_attachment_writes == 0 {
            "Attach".into()
        } else {
            format!("Attach ({} saving)", self.pending_attachment_writes)
        };
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
                    pill("attach", attach_label)
                        .when(attachments_full, |pill| {
                            pill.text_color(theme::faint())
                                .border_color(theme::line().opacity(0.5))
                                .cursor_default()
                        })
                        .tooltip(tip_with_shortcut(
                            if attachments_full {
                                "Attachment limit reached"
                            } else {
                                "Attach reference images"
                            },
                            Some("⌘O"),
                        ))
                        .when(!attachments_full, |pill| {
                            pill.on_click(cx.listener(|this, _, window, cx| {
                                this.add_attachment(&AddAttachment, window, cx)
                            }))
                        }),
                )
                .child(div().flex_1().child(self.prompt.clone()))
                .child(
                    div()
                        .id("send")
                        .role(Role::Button)
                        .aria_label(if submission_pending {
                            "Starting generation"
                        } else {
                            "Generate"
                        })
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
                            } else if submission_pending {
                                "Starting generation"
                            } else {
                                "Describe an image first"
                            },
                            Some("↵"),
                        ))
                        .child("↑")
                        .when(ready, |send| {
                            send.on_click(cx.listener(|this, _, window, cx| {
                                this.generate(&Generate, window, cx)
                            }))
                        }),
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
                    .role(Role::Button)
                    .aria_label(format!("Use sample prompt: {sample}"))
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
            .accessibility_id("codex-image.board-switcher")
            .role(Role::Button)
            .aria_label(format!("Switch board. Current board: {title}"))
            .aria_keyshortcuts("Meta+K")
            .absolute()
            .top(px(14.))
            .left(px(if cfg!(target_os = "macos") { 78. } else { 18. }))
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
                    .accessibility_id("codex-image.gallery.open")
                    .role(Role::Button)
                    .aria_label("Open image gallery")
                    .aria_keyshortcuts("G")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_save_honors_reserved_attachment_slots() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let images: Arc<[Image]> = vec![
            Image {
                format: ImageFormat::Png,
                bytes: vec![1, 2, 3],
                id: 1,
            },
            Image {
                format: ImageFormat::Jpeg,
                bytes: vec![4, 5, 6],
                id: 2,
            },
        ]
        .into();

        let paths =
            save_pending_clipboard_images(directory.path().to_owned(), Arc::clone(&images), 1)
                .expect("save clipboard image");

        assert_eq!(paths.len(), 1);
        assert_eq!(
            paths[0].extension().and_then(|value| value.to_str()),
            Some("png")
        );
        assert_eq!(fs::read(&paths[0]).expect("saved bytes"), images[0].bytes);
    }
}
