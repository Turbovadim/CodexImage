//! Everything layered over the canvas: the board switcher, the gallery, the
//! prompt and rename modals, the quit confirmation, and toasts.

use super::app::AppView;
use super::app::Overlay;
use super::composer::control_button;
use super::format::{format_date, format_tokens, node_depths, status_label, time_ago};
use super::input::TextInputMode;
use super::keymap::{Generate, OpenBoards, ToggleGallery};
use super::theme;
use super::tooltip::{tip, tip_with_shortcut};
use crate::model::BoardSummary;
use gpui::{
    AnyElement, ClipboardItem, Context, Focusable, FontWeight, ObjectFit, Role, SharedString,
    StyledImage, WeakEntity, Window, div, img, list, prelude::*, px,
};
use std::path::PathBuf;
use std::time::Duration;

pub(super) struct Toast {
    pub(super) text: String,
    pub(super) error: bool,
    pub(super) undo: Option<(String, String)>,
    pub(super) serial: u64,
}

/// One board-switcher row, resolved when the switcher opens. Building these
/// walks every node of every board and clones its title, which is far too much
/// to repeat per frame while a generation drives 30 fps repaints.
pub(super) struct BoardRow {
    summary: BoardSummary,
    /// The lower-cased title, so filtering never re-cases every title.
    search_key: String,
    thumbnail: Option<PathBuf>,
}

impl BoardRow {
    pub(super) fn new(summary: BoardSummary, view: &AppView) -> Self {
        // Resolved against the summary's own board. The switcher lists boards
        // that are not open, and the view's image assets only cover the open
        // one, so asking it would leave every other row's thumbnail blank.
        let repository = view.engine.repository();
        let thumbnail = summary.last_image.as_deref().and_then(|url| {
            repository
                .thumbnail_path(&summary.id, url)
                .filter(|path| path.exists())
                .or_else(|| repository.image_path(&summary.id, url))
        });
        Self {
            search_key: summary.title.to_lowercase(),
            thumbnail,
            summary,
        }
    }
}

struct GalleryImage {
    url: String,
    thumbnail: PathBuf,
}

/// Immutable gallery presentation data. Keeping only what the row paints
/// avoids duplicating node attempts, logs, labels, and source images.
pub(super) struct GalleryRow {
    node_id: String,
    prompt: SharedString,
    status: SharedString,
    metadata: SharedString,
    indent: f32,
    images: Vec<GalleryImage>,
}

impl GalleryRow {
    pub(super) fn rows_for(view: &AppView) -> Vec<Self> {
        let Some(board) = view.board.as_ref() else {
            return Vec::new();
        };
        let depths = node_depths(board);
        let mut nodes: Vec<_> = board.nodes.iter().collect();
        nodes.sort_by_key(|node| (std::cmp::Reverse(node.created_at), &node.id));
        nodes
            .into_iter()
            .map(|node| {
                let depth = depths.get(&node.id).copied().unwrap_or(0);
                let status = status_label(node);
                Self {
                    node_id: node.id.clone(),
                    prompt: node.prompt.clone().into(),
                    metadata: format!(
                        "{} · {} · {} branch depth",
                        status,
                        format_date(node.created_at),
                        depth
                    )
                    .into(),
                    status: status.into(),
                    indent: depth as f32 * 18.,
                    images: node
                        .images
                        .iter()
                        .map(|url| GalleryImage {
                            url: url.clone(),
                            thumbnail: view.display_image_path(url, false),
                        })
                        .collect(),
                }
            })
            .collect()
    }
}

fn render_gallery_row(
    row: &GalleryRow,
    view: WeakEntity<AppView>,
    available_width: f32,
) -> AnyElement {
    let mut strip = div().min_w_0().flex_1().flex().flex_wrap().gap_2();
    if row.images.is_empty() {
        strip = strip.child(
            div()
                .h(px(96.))
                .flex_1()
                .rounded_lg()
                .border_1()
                .border_color(theme::line())
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(theme::faint())
                .child(row.status.clone()),
        );
    } else {
        for (index, image) in row.images.iter().enumerate() {
            let node_id = row.node_id.clone();
            let image_url = image.url.clone();
            let image_view = view.clone();
            strip = strip.child(
                div()
                    .relative()
                    .child(
                        img(image.thumbnail.clone())
                            .id(SharedString::from(format!(
                                "gallery-image-{}-{index}",
                                row.node_id
                            )))
                            .role(Role::Button)
                            .aria_label(format!("Open image {} of {}", index + 1, row.images.len()))
                            .size(px(148.))
                            .rounded_lg()
                            .object_fit(ObjectFit::Cover)
                            .cursor_pointer()
                            .on_click(move |_, window, cx| {
                                let _ = image_view.update(cx, |this, cx| {
                                    this.open_lightbox(
                                        node_id.clone(),
                                        image_url.clone(),
                                        window,
                                        cx,
                                    );
                                });
                            }),
                    )
                    .when(row.images.len() > 1, |cell| {
                        cell.child(
                            div()
                                .absolute()
                                .right_1()
                                .bottom_1()
                                .rounded_md()
                                .bg(theme::background().opacity(0.78))
                                .px_1()
                                .text_xs()
                                .text_color(theme::ink())
                                .child(format!("{}/{}", index + 1, row.images.len())),
                        )
                    }),
            );
        }
    }

    let locate_id = row.node_id.clone();
    let locate_view = view;
    div()
        .w(px(available_width.max(0.)))
        .min_w_0()
        .border_b_1()
        .border_color(theme::line().opacity(0.7))
        .py_4()
        .flex()
        .gap_5()
        .child(
            div()
                .w(px(330.))
                .pl(px(row.indent))
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::ink())
                        .child(row.prompt.clone()),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(theme::faint())
                        .child(row.metadata.clone()),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("locate-{}", row.node_id)))
                        .role(Role::Button)
                        .aria_label("Show this generation on the canvas")
                        .mt_2()
                        .text_xs()
                        .text_color(theme::accent())
                        .cursor_pointer()
                        .hover(|style| style.text_color(theme::ink()))
                        .child("◎ Show on canvas")
                        .on_click(move |_, window, cx| {
                            let _ = locate_view.update(cx, |this, cx| {
                                this.locate_node(&locate_id, window, cx);
                            });
                        }),
                ),
        )
        .child(strip)
        .into_any_element()
}

impl AppView {
    pub(super) fn show_error(&mut self, error: impl std::fmt::Display, cx: &mut Context<Self>) {
        self.show_toast(error.to_string(), true, None, cx);
    }

    pub(super) fn show_toast(
        &mut self,
        text: String,
        error: bool,
        undo: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        // Confirmations get out of the way quickly; anything the user may want to
        // read or act on stays long enough to be useful.
        let lifetime = match (error, undo.is_some()) {
            (_, true) => 20,
            (true, _) => 14,
            _ => 5,
        };
        self.toast_serial += 1;
        let serial = self.toast_serial;
        self.toast = Some(Toast {
            text,
            error,
            undo,
            serial,
        });
        cx.spawn(async move |weak, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(lifetime))
                .await;
            let _ = weak.update(cx, |view, cx| {
                if view
                    .toast
                    .as_ref()
                    .is_some_and(|toast| toast.serial == serial)
                {
                    view.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = Overlay::None;
        self.armed_board_delete = None;
        self.modal_input.update(cx, |input, cx| input.clear(cx));
        window.focus(&self.focus, cx);
    }

    pub(super) fn open_boards(
        &mut self,
        _: &OpenBoards,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.overlay = if matches!(self.overlay, Overlay::Boards) {
            Overlay::None
        } else {
            Overlay::Boards
        };
        self.armed_board_delete = None;
        self.search_input.update(cx, |input, cx| input.clear(cx));
        self.refresh_overlay_data();
        if matches!(self.overlay, Overlay::Boards) {
            window.focus(&self.search_input.focus_handle(cx), cx);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    pub(super) fn toggle_gallery(
        &mut self,
        _: &ToggleGallery,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            self.overlay,
            Overlay::Lightbox(_)
                | Overlay::EditNode(_)
                | Overlay::RenameBoard(_)
                | Overlay::NodeText(_)
                | Overlay::QuitConfirm
        ) {
            return;
        }
        self.overlay = if matches!(self.overlay, Overlay::Gallery) {
            Overlay::None
        } else {
            Overlay::Gallery
        };
        self.refresh_overlay_data();
        cx.notify();
    }

    pub(super) fn save_edited_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Overlay::EditNode(node_id) = &self.overlay else {
            return;
        };
        let node_id = node_id.clone();
        let prompt = self.modal_input.read(cx).content().trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        match self.board_id().map(str::to_owned).and_then(|board_id| {
            self.engine
                .regenerate(&board_id, &node_id, Some(prompt), None)
        }) {
            Ok(()) => {
                self.overlay = Overlay::None;
                window.focus(&self.focus, cx);
            }
            Err(error) => self.show_error(error, cx),
        }
        cx.notify();
    }

    pub(super) fn rename_open_board(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Overlay::RenameBoard(board_id) = &self.overlay else {
            return;
        };
        let board_id = board_id.clone();
        let title = self.modal_input.read(cx).content().trim().to_owned();
        match self.engine.repository().rename_board(&board_id, &title) {
            Ok(()) => {
                self.overlay = Overlay::Boards;
                self.refresh_overlay_data();
                window.focus(&self.search_input.focus_handle(cx), cx);
            }
            Err(error) => self.show_error(error, cx),
        }
        cx.notify();
    }

    pub(super) fn render_boards(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let query = self.search_input.read(cx).content().to_lowercase();
        let width = 360.;
        let mut list = div()
            .id("board-list")
            .max_h(px(
                (f32::from(window.viewport_size().height) * 0.58).max(300.)
            ))
            .overflow_y_scroll()
            .px_2()
            .pb_2();
        for row in self
            .board_rows
            .iter()
            .filter(|row| row.search_key.contains(&query))
        {
            let summary = &row.summary;
            let id = summary.id.clone();
            let rename_id = summary.id.clone();
            let delete_id = summary.id.clone();
            let active = self.board_id.as_deref() == Some(summary.id.as_str());
            let armed = self.armed_board_delete.as_deref() == Some(summary.id.as_str());
            let mut thumbnail = div()
                .size(px(34.))
                .rounded_md()
                .border_1()
                .border_color(theme::line())
                .bg(theme::background())
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme::faint())
                .child("❖")
                .into_any_element();
            if let Some(path) = &row.thumbnail {
                thumbnail = img(path.clone())
                    .size(px(34.))
                    .rounded_md()
                    .object_fit(ObjectFit::Cover)
                    .into_any_element();
            }
            list = list.child(
                div()
                    .id(SharedString::from(format!("board-{}", summary.id)))
                    .role(Role::Button)
                    .aria_label(format!("Open board {}", summary.title))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_lg()
                    .px_2()
                    .py_2()
                    .bg(if active {
                        theme::hover()
                    } else {
                        theme::raised()
                    })
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::hover()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_board(id.clone(), window, cx)
                    }))
                    .child(thumbnail)
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(if active { theme::ink() } else { theme::dim() })
                                    .child(summary.title.clone()),
                            )
                            .child(div().text_xs().text_color(theme::faint()).child(format!(
                                "{} images · {} tok · {}",
                                summary.image_count,
                                format_tokens(summary.total_tokens),
                                time_ago(summary.updated_at)
                            ))),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("rename-{}", rename_id)))
                            .role(Role::Button)
                            .aria_label(format!("Rename board {}", summary.title))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .text_color(theme::faint())
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::hover()).text_color(theme::ink()))
                            .tooltip(tip("Rename this board"))
                            .child("Rename")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                let title = this
                                    .engine
                                    .repository()
                                    .board(&rename_id)
                                    .map(|board| board.title)
                                    .unwrap_or_default();
                                this.modal_input.update(cx, |input, cx| {
                                    input.set_mode(TextInputMode::SingleLine, cx);
                                    input.set_placeholder("Board name", cx);
                                    input.set_content(title, cx);
                                });
                                this.overlay = Overlay::RenameBoard(rename_id.clone());
                                window.focus(&this.modal_input.focus_handle(cx), cx);
                            })),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("delete-board-{}", delete_id)))
                            .role(Role::Button)
                            .aria_label(if armed {
                                format!("Confirm deleting board {}", summary.title)
                            } else {
                                format!("Delete board {}", summary.title)
                            })
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .text_color(theme::danger())
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::danger().opacity(0.16)))
                            .tooltip(tip(if armed {
                                "Click again to delete this board for good"
                            } else {
                                "Delete this board"
                            }))
                            .when(armed, |button| button.bg(theme::danger().opacity(0.16)))
                            .child(if armed { "Sure?" } else { "Delete" })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                if this.armed_board_delete.as_deref() == Some(&delete_id) {
                                    match this.engine.delete_board(&delete_id) {
                                        Ok(()) => {
                                            this.clear_render_caches(window, cx);
                                            let next = this
                                                .engine
                                                .repository()
                                                .summaries()
                                                .first()
                                                .map(|summary| summary.id.clone());
                                            this.board_id = next.clone();
                                            this.board = next
                                                .as_deref()
                                                .and_then(|id| this.engine.repository().board(id));
                                            this.armed_board_delete = None;
                                            this.refresh_layout();
                                        }
                                        Err(error) => this.show_error(error, cx),
                                    }
                                } else {
                                    this.armed_board_delete = Some(delete_id.clone());
                                }
                                cx.notify();
                            })),
                    ),
            );
        }
        div()
            .id("boards-popover")
            .absolute()
            .top(px(58.))
            .left(px(18.))
            .w(px(width))
            .rounded_xl()
            .border_1()
            .border_color(theme::line())
            .bg(theme::raised().opacity(0.98))
            .overflow_hidden()
            .occlude()
            .child(
                div().p_3().child(
                    div()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme::line())
                        .bg(theme::background())
                        .px_3()
                        .py_2()
                        .child(self.search_input.clone()),
                ),
            )
            .child(list)
            .child(
                div()
                    .id("new-board")
                    .role(Role::Button)
                    .aria_label("Create a new board")
                    .border_t_1()
                    .border_color(theme::line())
                    .px_4()
                    .py_3()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::accent())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::hover()))
                    .child("＋ New board")
                    .on_click(cx.listener(|this, _, window, cx| {
                        match this.engine.repository().create_board() {
                            Ok(board) => this.open_board(board.id, window, cx),
                            Err(error) => this.show_error(error, cx),
                        }
                    })),
            )
            .into_any_element()
    }

    pub(super) fn render_gallery(&self, cx: &mut Context<Self>) -> AnyElement {
        let board = self.board.as_ref();
        let image_count: usize = board
            .map(|board| board.nodes.iter().map(|node| node.images.len()).sum())
            .unwrap_or(0);
        let node_count = board.map(|board| board.nodes.len()).unwrap_or(0);
        let rows = self.gallery_rows.clone();
        let view = cx.weak_entity();
        let content = div().w_full().flex_1().min_h_0().px_6().pb_8().child(
            list(
                self.gallery_list_state.clone(),
                move |index, window, _cx| {
                    render_gallery_row(
                        &rows[index],
                        view.clone(),
                        f32::from(window.viewport_size().width) - 48.,
                    )
                },
            )
            .w_full()
            .h_full(),
        );
        div()
            .id("gallery")
            .accessibility_id("codex-image.gallery")
            .role(Role::Dialog)
            .aria_label("Image gallery")
            .absolute()
            .inset_0()
            .key_context("CodexImageGallery")
            .bg(theme::background())
            .flex()
            .flex_col()
            .occlude()
            .child(
                div()
                    .flex_none()
                    .border_b_1()
                    .border_color(theme::line())
                    .px_6()
                    .py_4()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .size(px(28.))
                            .rounded_lg()
                            .bg(theme::accent().opacity(0.12))
                            .text_color(theme::accent())
                            .flex()
                            .items_center()
                            .justify_center()
                            .child("⑂"),
                    )
                    .child(
                        div()
                            .ml_3()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::ink())
                                    .child("Branch gallery"),
                            )
                            .child(
                                div().text_xs().text_color(theme::dim()).child(format!(
                                    "{image_count} images · {node_count} generations"
                                )),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("close-gallery")
                            .accessibility_id("codex-image.gallery.close")
                            .role(Role::Button)
                            .aria_label("Close image gallery")
                            .size(px(32.))
                            .rounded_lg()
                            .border_1()
                            .border_color(theme::line())
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme::dim())
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::hover()).text_color(theme::ink()))
                            .tooltip(tip_with_shortcut("Close the gallery", Some("Esc")))
                            .child("×")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_overlay(window, cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(content)
            .into_any_element()
    }

    pub(super) fn render_modal(
        &self,
        title: &str,
        detail: &str,
        action: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("modal-overlay")
            .role(Role::Dialog)
            .aria_label(title.to_owned())
            .absolute()
            .inset_0()
            .bg(gpui::black().opacity(0.72))
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .child(
                div()
                    .w(px(560.))
                    .rounded_xl()
                    .border_1()
                    .border_color(theme::line())
                    .bg(theme::raised())
                    .p_5()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::ink())
                            .child(title.to_owned()),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_sm()
                            .text_color(theme::dim())
                            .child(detail.to_owned()),
                    )
                    .child(
                        div()
                            .mt_4()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme::line())
                            .bg(theme::background())
                            .px_3()
                            .py_2()
                            .child(self.modal_input.clone()),
                    )
                    .child(
                        div()
                            .mt_5()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(control_button(
                                "Cancel",
                                cx.listener(|this, _, window, cx| {
                                    this.close_overlay(window, cx);
                                    cx.notify();
                                }),
                            ))
                            .child(
                                div()
                                    .id("modal-submit")
                                    .role(Role::Button)
                                    .aria_label(action.to_owned())
                                    .rounded_lg()
                                    .bg(theme::accent_strong())
                                    .px_4()
                                    .py_2()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(gpui::white())
                                    .cursor_pointer()
                                    .child(action.to_owned())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.generate(&Generate, window, cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The full text and error message of one node, shown when its truncated
    /// status line is clicked.
    pub(super) fn render_node_text(&self, node_id: &str, cx: &mut Context<Self>) -> AnyElement {
        let node = self.node(node_id);
        let error = node
            .as_ref()
            .and_then(|node| node.error.clone())
            .filter(|error| !error.is_empty());
        let text = node
            .map(|node| node.text)
            .filter(|text| !text.is_empty())
            // The card would only offer this popup for one of the two, but a
            // stale click can race a refresh; degrade to a placeholder.
            .or_else(|| error.is_none().then(|| "No message was recorded.".into()));
        let copy_text = text.clone();
        let mut body = div()
            .id("node-text-body")
            .mt_3()
            .max_h(px(420.))
            .overflow_y_scroll()
            .rounded_lg()
            .border_1()
            .border_color(theme::line())
            .bg(theme::background())
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_3();
        if let Some(error) = error {
            body = body.child(div().text_sm().text_color(theme::danger()).child(error));
        }
        if let Some(text) = text {
            body = body.child(div().text_sm().text_color(theme::ink()).child(text));
        }
        let mut buttons = div().mt_4().flex().justify_end().gap_2();
        if let Some(copy_text) = copy_text {
            buttons = buttons.child(control_button(
                "Copy",
                cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                    this.show_toast("Message copied".into(), false, None, cx);
                }),
            ));
        }
        buttons = buttons.child(control_button(
            "Close",
            cx.listener(|this, _, window, cx| {
                this.close_overlay(window, cx);
                cx.notify();
            }),
        ));
        div()
            .id("node-text-overlay")
            .absolute()
            .inset_0()
            .bg(gpui::black().opacity(0.72))
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .on_click(cx.listener(|this, _, window, cx| {
                this.close_overlay(window, cx);
                cx.notify();
            }))
            .child(
                div()
                    .id("node-text-panel")
                    .role(Role::Dialog)
                    .aria_label("Codex message")
                    .w(px(560.))
                    .rounded_xl()
                    .border_1()
                    .border_color(theme::line())
                    .bg(theme::raised())
                    .p_5()
                    .occlude()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::ink())
                            .child("Codex message"),
                    )
                    .child(body)
                    .child(buttons),
            )
            .into_any_element()
    }

    pub(super) fn render_quit_confirm(&self, cx: &mut Context<Self>) -> AnyElement {
        let count = self.engine.active_count();
        div()
            .id("quit-confirm-overlay")
            .absolute()
            .inset_0()
            .bg(gpui::black().opacity(0.72))
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .child(
                div()
                    .id("quit-confirm-dialog")
                    .w(px(480.))
                    .role(Role::Dialog)
                    .aria_label("Generations are still running")
                    .rounded_xl()
                    .border_1()
                    .border_color(theme::line())
                    .bg(theme::raised())
                    .p_5()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::ink())
                            .child("Generations are still running"),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_sm()
                            .text_color(theme::dim())
                            .child(format!(
                                "{count} generation(s) are still running. Quitting will terminate them; images already received will be kept."
                            )),
                    )
                    .child(
                        div()
                            .mt_5()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(control_button(
                                "Keep running",
                                cx.listener(|this, _, window, cx| {
                                    this.close_overlay(window, cx);
                                    cx.notify();
                                }),
                            ))
                            .child(
                                div()
                                    .id("terminate-quit")
                                    .role(Role::Button)
                                    .aria_label("Terminate generations and quit")
                                    .rounded_lg()
                                    .bg(theme::danger().opacity(0.15))
                                    .px_4()
                                    .py_2()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::danger())
                                    .cursor_pointer()
                                    .child("Terminate and quit")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.engine.stop_all_for_quit();
                                        cx.quit();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_toast(
        &self,
        toast: &Toast,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = div()
            .id("toast")
            .absolute()
            .top(px(20.))
            .left(px((f32::from(window.viewport_size().width) - 430.) / 2.))
            .w(px(430.))
            .rounded_xl()
            .border_1()
            .border_color(if toast.error {
                theme::danger().opacity(0.55)
            } else {
                theme::line()
            })
            .bg(theme::raised().opacity(0.98))
            .px_4()
            .py_3()
            .flex()
            .items_center()
            .gap_3()
            .occlude()
            .text_sm()
            .text_color(if toast.error {
                theme::danger()
            } else {
                theme::ink()
            })
            .child(div().flex_1().child(toast.text.clone()));
        if let Some((board_id, undo_id)) = &toast.undo {
            let board_id = board_id.clone();
            let undo_id = undo_id.clone();
            row = row.child(
                div()
                    .id("undo")
                    .role(Role::Button)
                    .aria_label("Undo deletion")
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::accent_strong())
                    .px_3()
                    .py_1()
                    .text_color(theme::accent())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::accent_strong().opacity(0.2)))
                    .child("Undo")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        match this.engine.repository().undo_delete(&board_id, &undo_id) {
                            Ok(_) => this.toast = None,
                            Err(error) => this.show_error(error, cx),
                        }
                        cx.notify();
                    })),
            );
        }
        row.child(
            div()
                .id("dismiss-toast")
                .role(Role::Button)
                .aria_label("Dismiss notification")
                .px_1()
                .rounded_md()
                .text_color(theme::faint())
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover()).text_color(theme::ink()))
                .tooltip(tip("Dismiss"))
                .child("×")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toast = None;
                    cx.notify();
                })),
        )
        .into_any_element()
    }
}
