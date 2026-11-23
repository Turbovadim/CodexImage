//! The per-card conversation: ask Codex what it thinks about one result, with
//! that card's chain, images, summary, and failure already in the prompt.

use super::app::{AppView, Overlay};
use super::dialog::{header, panel, present, scrim, title};
use super::icons::{Icon, icon};
use super::theme;
use super::tooltip::{tip, tip_with_shortcut};
use crate::generation::chat_activity_key;
use crate::model::{ChatMessage, ChatRole};
use gpui::{
    AnyElement, Context, Focusable, ObjectFit, Role, SharedString, StyledImage, Window, div, img,
    list, prelude::*, px,
};

const PANEL_WIDTH: f32 = 720.;
const BUBBLE_WIDTH: f32 = 520.;
const REPLY_WIDTH: f32 = 620.;

/// The user's turns sit in a bubble on the right; Codex answers as plain text
/// on the left, so long replies read like a page instead of a stack of boxes.
fn render_message(message: &ChatMessage) -> AnyElement {
    let row = div().w_full().py_2().flex().text_sm();
    match message.role {
        ChatRole::User => row.justify_end().child(
            div()
                .max_w(px(BUBBLE_WIDTH))
                .rounded_xl()
                .bg(theme::hover())
                .px_3()
                .py_2()
                .text_color(theme::ink())
                .child(message.text.clone()),
        ),
        ChatRole::Agent => row.child(
            div()
                .max_w(px(REPLY_WIDTH))
                .text_color(theme::ink())
                .child(message.text.clone()),
        ),
        ChatRole::Error => row.child(
            div()
                .max_w(px(REPLY_WIDTH))
                .text_color(theme::danger())
                .child(message.text.clone()),
        ),
    }
    .into_any_element()
}

impl AppView {
    pub(super) fn open_chat(&mut self, node_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.node(node_id).is_none() {
            return;
        }
        self.overlay = Overlay::Chat(node_id.to_owned());
        self.refresh_overlay_data(cx);
        window.focus(&self.chat_input.focus_handle(cx), cx);
        cx.notify();
    }

    pub(super) fn send_chat_message(&mut self, cx: &mut Context<Self>) {
        let Overlay::Chat(node_id) = &self.overlay else {
            return;
        };
        let node_id = node_id.clone();
        let message = self.chat_input.read(cx).content().trim().to_owned();
        if message.is_empty() {
            return;
        }
        let board_id = match self.board_id() {
            Ok(id) => id.to_owned(),
            Err(error) => {
                self.show_error(error, cx);
                return;
            }
        };
        match self.engine.send_chat(&board_id, &node_id, message) {
            Ok(()) => self.chat_input.update(cx, |input, cx| input.clear(cx)),
            Err(error) => self.show_error(error, cx),
        }
        cx.notify();
    }

    pub(super) fn render_chat(
        &self,
        node_id: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(node) = self.node(node_id) else {
            return div().into_any_element();
        };
        let thinking = self.engine.is_chatting(node_id);
        let activity = self
            .activity
            .get(&chat_activity_key(node_id))
            .filter(|text| !text.is_empty())
            .cloned();
        let messages = self.chat_rows.clone();
        let height = (f32::from(window.viewport_size().height) * 0.78).max(420.);
        let transcript = if messages.is_empty() {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .text_sm()
                .text_color(theme::faint())
                .child(icon(Icon::Chat, 22.).text_color(theme::faint()))
                .child("Ask why it came out this way, or what to try next")
                .into_any_element()
        } else {
            list(self.chat_list_state.clone(), move |index, _window, _cx| {
                render_message(&messages[index])
            })
            .size_full()
            .into_any_element()
        };
        let ready = !self.chat_input.read(cx).content().trim().is_empty() && !thinking;
        let focused = self.chat_input.focus_handle(cx).is_focused(window);
        // One line: line breaks in the prompt would defeat the truncation.
        let prompt: String = node
            .prompt
            .chars()
            .take(200)
            .map(|character| {
                if character.is_whitespace() {
                    ' '
                } else {
                    character
                }
            })
            .collect();
        let lead = div()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(10.))
            .children(node.images.first().map(|url| {
                img(self.sprite_image_path(url))
                    .size(px(26.))
                    .flex_none()
                    .rounded_md()
                    .object_fit(ObjectFit::Cover)
            }))
            .child(title("Chat"))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(theme::dim())
                    .child(prompt),
            );
        let action = if thinking {
            let stop_node = node_id.to_owned();
            div()
                .id("stop-chat")
                .role(Role::Button)
                .aria_label("Stop this answer")
                .bg(theme::danger().opacity(0.16))
                .cursor_pointer()
                .hover(|style| style.bg(theme::danger().opacity(0.28)))
                .tooltip(tip("Stop this answer"))
                .child(icon(Icon::Stop, 14.).text_color(theme::danger()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.engine.stop_chat(&stop_node);
                    cx.notify();
                }))
        } else {
            div()
                .id("send-chat")
                .role(Role::Button)
                .aria_label("Send message")
                .tooltip(tip_with_shortcut("Send · ⇧↵ for a new line", Some("↵")))
                .child(icon(Icon::ArrowUp, 15.).text_color(if ready {
                    gpui::white()
                } else {
                    theme::faint()
                }))
                .when(ready, |send| {
                    send.bg(theme::accent_strong())
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::accent()))
                        .on_click(cx.listener(|this, _, _, cx| this.send_chat_message(cx)))
                })
                .when(!ready, |send| send.bg(theme::hover()))
        };
        let panel = panel("chat-panel", "Chat about this card", PANEL_WIDTH)
            .h(px(height))
            .child(header(lead, None, cx))
            .child(div().flex_1().min_h_0().px_4().py_2().child(transcript))
            .when(thinking, |panel| {
                panel.child(
                    div()
                        .flex_none()
                        .px_5()
                        .pb_2()
                        .truncate()
                        .text_xs()
                        .text_color(theme::faint())
                        .child(SharedString::from(
                            activity.unwrap_or_else(|| "Thinking".to_owned()),
                        )),
                )
            })
            .child(
                div()
                    .flex_none()
                    .mx_4()
                    .mb_4()
                    .flex()
                    .items_end()
                    .gap_2()
                    .rounded_xl()
                    .border_1()
                    .border_color(if focused {
                        theme::accent().opacity(0.55)
                    } else {
                        theme::line()
                    })
                    .bg(theme::background())
                    .pl_3()
                    .pr_1p5()
                    .py_1p5()
                    .child(div().flex_1().min_w_0().child(self.chat_input.clone()))
                    .child(
                        action
                            .size(px(30.))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .rounded_full(),
                    ),
            );
        present(
            scrim("chat-overlay")
                .on_click(cx.listener(|this, _, window, cx| this.close_overlay(window, cx))),
            panel,
        )
    }
}
