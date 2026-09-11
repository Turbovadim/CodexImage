//! The settings overlay: which model this app asks Codex for, how hard it
//! thinks, and how much of a card's chain travels with a generation.

use super::app::{AppView, Overlay};
use super::composer::control_button;
use super::theme;
use super::tooltip::tip;
use crate::settings::{MAX_LINEAGE_REFS, MODEL_PRESETS, REASONING_EFFORTS, SettingsData};
use gpui::{
    AnyElement, Context, Focusable, FontWeight, Role, SharedString, Window, div, prelude::*, px,
};

fn effort_label(effort: &str) -> &str {
    if effort.is_empty() {
        "CLI default"
    } else {
        effort
    }
}

fn row(label: &'static str, detail: &'static str, control: AnyElement) -> AnyElement {
    div()
        .mt_4()
        .flex()
        .items_start()
        .gap_4()
        .child(
            div()
                .w(px(180.))
                .flex_none()
                .child(div().text_sm().text_color(theme::ink()).child(label))
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(theme::faint())
                        .child(detail),
                ),
        )
        .child(div().flex_1().min_w_0().child(control))
        .into_any_element()
}

impl AppView {
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.engine.settings().get().model;
        self.modal_input.update(cx, |input, cx| {
            input.set_mode(super::input::TextInputMode::SingleLine, cx);
            input.set_placeholder("Model id, blank for the CLI default…", cx);
            input.set_content(model, cx);
        });
        self.overlay = Overlay::Settings;
        window.focus(&self.modal_input.focus_handle(cx), cx);
        cx.notify();
    }

    /// Writes one settings change and reports a failed save instead of letting
    /// the UI drift away from what the next generation would actually use.
    pub(super) fn change_settings(
        &mut self,
        change: impl FnOnce(&mut SettingsData),
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.engine.settings().update(change) {
            self.show_error(error, cx);
        }
        cx.notify();
    }

    pub(super) fn save_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.modal_input.read(cx).content().trim().to_owned();
        self.change_settings(|settings| settings.model = model, cx);
        self.close_overlay(window, cx);
        cx.notify();
    }

    pub(super) fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = self.engine.settings().get();
        let pill = |id: SharedString, label: String, active: bool| {
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(label.clone())
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(if active {
                    theme::accent()
                } else {
                    theme::line()
                })
                .text_xs()
                .text_color(if active { theme::ink() } else { theme::dim() })
                .cursor_pointer()
                .hover(|style| style.border_color(theme::faint()).text_color(theme::ink()))
                .child(label)
        };
        let mut presets = div().flex().flex_wrap().gap_2().child(
            pill(
                "model-default".into(),
                "CLI default".into(),
                settings.model.is_empty(),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.modal_input
                    .update(cx, |input, cx| input.set_content("", cx));
                this.change_settings(|settings| settings.model.clear(), cx);
            })),
        );
        for preset in MODEL_PRESETS {
            presets = presets.child(
                pill(
                    SharedString::from(format!("model-{preset}")),
                    (*preset).to_owned(),
                    settings.model == *preset,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.modal_input
                        .update(cx, |input, cx| input.set_content(*preset, cx));
                    this.change_settings(|settings| settings.model = (*preset).to_owned(), cx);
                })),
            );
        }
        let effort = settings.reasoning_effort.clone();
        let lineage = settings.lineage_refs;
        div()
            .id("settings-overlay")
            .absolute()
            .inset_0()
            .bg(gpui::black().opacity(0.72))
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .child(
                div()
                    .id("settings-panel")
                    .role(Role::Dialog)
                    .aria_label("Codex settings")
                    .w(px(680.))
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
                            .child("Codex settings"),
                    )
                    .child(div().mt_1().text_sm().text_color(theme::dim()).child(
                        "These apply to this app only. Your Codex CLI config is left alone.",
                    ))
                    .child(row(
                        "Model",
                        "Passed as codex exec -m",
                        div()
                            .child(
                                div()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(theme::line())
                                    .bg(theme::background())
                                    .px_3()
                                    .py_2()
                                    .child(self.modal_input.clone()),
                            )
                            .child(div().mt_2().child(presets))
                            .into_any_element(),
                    ))
                    .child(row(
                        "Reasoning effort",
                        "Passed as model_reasoning_effort",
                        pill(
                            "effort".into(),
                            effort_label(&effort).to_owned(),
                            !effort.is_empty(),
                        )
                        .tooltip(tip("Click to cycle"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let next = REASONING_EFFORTS
                                .iter()
                                .position(|value| *value == effort)
                                .map_or(0, |index| (index + 1) % REASONING_EFFORTS.len());
                            this.change_settings(
                                |settings| {
                                    settings.reasoning_effort = REASONING_EFFORTS[next].to_owned()
                                },
                                cx,
                            );
                        }))
                        .into_any_element(),
                    ))
                    .child(row(
                        "Chain references",
                        "Earlier images from a card's own chain sent along as identity reference",
                        pill(
                            "lineage".into(),
                            if lineage == 0 {
                                "Off".to_owned()
                            } else {
                                format!("{lineage} image(s)")
                            },
                            lineage > 0,
                        )
                        .tooltip(tip("Click to cycle"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let next = (lineage + 1) % (MAX_LINEAGE_REFS + 1);
                            this.change_settings(|settings| settings.lineage_refs = next, cx);
                        }))
                        .into_any_element(),
                    ))
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
                                    .id("settings-save")
                                    .role(Role::Button)
                                    .aria_label("Save settings")
                                    .rounded_lg()
                                    .bg(theme::accent_strong())
                                    .px_4()
                                    .py_2()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(gpui::white())
                                    .cursor_pointer()
                                    .child("Save")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_model(window, cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
