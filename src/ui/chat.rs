//! The per-card conversation: ask Codex what it thinks about one result, with
//! that card's chain, images, summary, and failure already in the prompt.

use super::app::{AppView, Overlay};
use super::composer::control_button;
use super::theme;
use super::tooltip::tip;
use crate::generation::chat_activity_key;
use crate::model::{ChatMessage, ChatRole};
use gpui::{
    AnyElement, Context, Focusable, FontWeight, Role, SharedString, Window, div, list, prelude::*,
    px,
};

const PANEL_WIDTH: f32 = 720.;
const BUBBLE_WIDTH: f32 = 560.;

fn render_message(message: &ChatMessage) -> AnyElement {
    let (align, background, color) = match message.role {
        ChatRole::User => (true, theme::hover(), theme::ink()),
        ChatRole::Agent => (false, theme::background(), theme::ink()),
        ChatRole::Error => (false, theme::background(), theme::danger()),
    };
    div()
        .w_full()
        .py_1p5()
        .flex()
        .when(align, |row| row.justify_end())
        .child(
            div()
                .max_w(px(BUBBLE_WIDTH))
                .rounded_lg()
                .border_1()
                .border_color(theme::line())
                .bg(background)
                .px_3()
                .py_2()
                .text_sm()
                .text_color(color)
                .child(message.text.clone()),
        )
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
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme::faint())
                .child("Ask what Codex thinks of this result, why it came out this way, or what to try next.")
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .child(
                    list(self.chat_list_state.clone(), move |index, _window, _cx| {
                        render_message(&messages[index])
                    })
                    .w_full()
                    .h_full(),
                )
                .into_any_element()
        };
        let ready = !self.chat_input.read(cx).content().trim().is_empty() && !thinking;
        let chat_node = node_id.to_owned();
        div()
            .id("chat-overlay")
            .absolute()
            .inset_0()
            .bg(gpui::black().opacity(0.72))
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .child(
                div()
                    .id("chat-panel")
                    .role(Role::Dialog)
                    .aria_label("Chat about this card")
                    .w(px(PANEL_WIDTH))
                    .h(px(height))
                    .rounded_xl()
                    .border_1()
                    .border_color(theme::line())
                    .bg(theme::raised())
                    .p_5()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_start()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme::ink())
                                            .child("Chat about this card"),
                                    )
                                    .child(
                                        div().mt_1().text_xs().text_color(theme::dim()).child(
                                            node.prompt.chars().take(160).collect::<String>(),
                                        ),
                                    ),
                            )
                            .child(control_button(
                                "Close",
                                cx.listener(|this, _, window, cx| {
                                    this.close_overlay(window, cx);
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(div().mt_3().flex_1().min_h_0().flex().child(transcript))
                    .when(thinking, |panel| {
                        let stop_node = chat_node.clone();
                        panel.child(
                            div()
                                .flex_none()
                                .mt_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .text_color(theme::faint())
                                .child(SharedString::from(
                                    activity.unwrap_or_else(|| "Thinking".to_owned()),
                                ))
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .id("stop-chat")
                                        .role(Role::Button)
                                        .aria_label("Stop this answer")
                                        .text_color(theme::danger())
                                        .cursor_pointer()
                                        .child("Stop")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.engine.stop_chat(&stop_node);
                                            cx.notify();
                                        })),
                                ),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .mt_3()
                            .flex()
                            .items_end()
                            .gap_2()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme::line())
                            .bg(theme::background())
                            .px_3()
                            .py_2()
                            .child(div().flex_1().child(self.chat_input.clone()))
                            .child(
                                div()
                                    .id("send-chat")
                                    .role(Role::Button)
                                    .aria_label("Send message")
                                    .size(px(28.))
                                    .rounded_lg()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(ready, |send| {
                                        send.bg(theme::accent_strong().opacity(0.18))
                                            .text_color(theme::accent())
                                            .cursor_pointer()
                                            .hover(|style| {
                                                style.bg(theme::accent_strong().opacity(0.32))
                                            })
                                    })
                                    .when(!ready, |send| {
                                        send.bg(theme::hover()).text_color(theme::faint())
                                    })
                                    .tooltip(tip("Send · ⇧↵ for a new line"))
                                    .child("↑")
                                    .when(ready, |send| {
                                        send.on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.send_chat_message(cx)
                                            }),
                                        )
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }
}
