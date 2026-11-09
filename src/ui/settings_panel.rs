//! The settings overlay: which model this app asks Codex for, how hard it
//! thinks, and how much of a card's chain travels with a generation.

use super::app::{AppView, Overlay};
use super::composer::control_button;
use super::theme;
use super::tooltip::tip;
use crate::generation::CodexModel;
use crate::settings::{MAX_LINEAGE_REFS, SettingsData};
use gpui::{
    AnyElement, Context, Focusable, FontWeight, Role, SharedString, Stateful, Window, div,
    prelude::*, px,
};
use std::sync::Arc;

fn pill(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    active: bool,
) -> Stateful<gpui::Div> {
    let label = label.into();
    div()
        .id(id.into())
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
        // The id field only shows while Codex has not listed any models.
        if self.codex_models.is_empty() {
            window.focus(&self.modal_input.focus_handle(cx), cx);
        }
        self.refresh_codex_models(cx);
        cx.notify();
    }

    /// Asks Codex for its models in the background. A refresh already in
    /// flight is reused, and a failed one keeps the last list.
    pub(super) fn refresh_codex_models(&mut self, cx: &mut Context<Self>) {
        if self.codex_models_task.is_some() {
            return;
        }
        let engine = self.engine.clone();
        self.codex_models_task = Some(cx.spawn(async move |weak, cx| {
            let models = cx
                .background_spawn(async move { engine.list_models() })
                .await;
            let _ = weak.update(cx, |view, cx| {
                view.codex_models_task = None;
                if let Ok(models) = models
                    && !models.is_empty()
                {
                    view.codex_models = Arc::new(models);
                }
                cx.notify();
            });
        }));
    }

    /// The listed model a generation would use: the chosen one, or the CLI's
    /// default when none is chosen.
    fn listed_model(&self, model: &str) -> Option<&CodexModel> {
        self.codex_models.iter().find(|listed| {
            if model.is_empty() {
                listed.is_default
            } else {
                listed.model == model
            }
        })
    }

    /// The header label: the model's display name plus any effort override.
    pub(super) fn model_summary(&self) -> String {
        let settings = self.engine.settings().get();
        let name = self
            .listed_model(&settings.model)
            .filter(|_| !settings.model.is_empty())
            .map_or(settings.model_label(), CodexModel::label);
        if settings.reasoning_effort.is_empty() {
            name.to_owned()
        } else {
            format!("{name} · {}", settings.reasoning_effort)
        }
    }

    /// Selects `model` (empty for the CLI default) and drops an effort override
    /// the new model does not offer.
    fn pick_model(&mut self, model: String, cx: &mut Context<Self>) {
        let effort = self.engine.settings().get().reasoning_effort;
        let keep_effort = effort.is_empty()
            || self
                .listed_model(&model)
                .is_none_or(|listed| listed.supports_effort(&effort));
        self.modal_input
            .update(cx, |input, cx| input.set_content(model.clone(), cx));
        self.change_settings(
            |settings| {
                settings.model = model;
                if !keep_effort {
                    settings.reasoning_effort.clear();
                }
            },
            cx,
        );
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
    }

    pub(super) fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = self.engine.settings().get();
        let active = self.listed_model(&settings.model);
        let model_control = self.render_model_choices(&settings, cx);
        let mut efforts = div().flex().flex_wrap().gap_2().child(
            pill(
                "effort-default",
                match active.and_then(|model| model.default_reasoning_effort.as_deref()) {
                    Some(effort) => format!("Default ({effort})"),
                    None => "CLI default".to_owned(),
                },
                settings.reasoning_effort.is_empty(),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.change_settings(|settings| settings.reasoning_effort.clear(), cx)
            })),
        );
        let listed_efforts = active.map_or(&[][..], |model| &model.supported_reasoning_efforts);
        for option in listed_efforts {
            let effort = option.reasoning_effort.clone();
            efforts = efforts.child(
                pill(
                    format!("effort-{effort}"),
                    effort.clone(),
                    settings.reasoning_effort == effort,
                )
                .tooltip(tip(option.description.clone()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let effort = effort.clone();
                    this.change_settings(|settings| settings.reasoning_effort = effort, cx)
                })),
            );
        }
        if !settings.reasoning_effort.is_empty()
            && listed_efforts
                .iter()
                .all(|option| option.reasoning_effort != settings.reasoning_effort)
        {
            efforts = efforts.child(pill(
                "effort-custom",
                settings.reasoning_effort.clone(),
                true,
            ));
        }
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
                    .child(row("Model", "Passed as codex exec -m", model_control))
                    .child(row(
                        "Reasoning effort",
                        "Passed as model_reasoning_effort",
                        efforts.into_any_element(),
                    ))
                    .child(row(
                        "Chain references",
                        "Earlier images from a card's own chain sent along as identity reference",
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .children((0..=MAX_LINEAGE_REFS).map(|count| {
                                pill(
                                    format!("lineage-{count}"),
                                    if count == 0 {
                                        "Off".to_owned()
                                    } else {
                                        count.to_string()
                                    },
                                    lineage == count,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.change_settings(
                                            |settings| settings.lineage_refs = count,
                                            cx,
                                        )
                                    },
                                ))
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

    /// One button per model Codex lists. Until it lists any, a free-text id
    /// field stands in so a missing or signed-out CLI never blocks the choice.
    fn render_model_choices(&self, settings: &SettingsData, cx: &mut Context<Self>) -> AnyElement {
        let mut choices = div().flex().flex_wrap().gap_2().child(
            pill("model-default", "CLI default", settings.model.is_empty())
                .on_click(cx.listener(|this, _, _, cx| this.pick_model(String::new(), cx))),
        );
        for model in self.codex_models.iter() {
            let id = model.model.clone();
            choices = choices.child(
                pill(
                    format!("model-{id}"),
                    model.label().to_owned(),
                    settings.model == id,
                )
                .tooltip(tip(if model.description.is_empty() {
                    id.clone()
                } else {
                    format!("{id}: {}", model.description)
                }))
                .on_click(cx.listener(move |this, _, _, cx| this.pick_model(id.clone(), cx))),
            );
        }
        if self.codex_models.is_empty() {
            let status = if self.codex_models_task.is_some() {
                "Loading models from Codex…"
            } else {
                "Codex did not list any models. Type an id instead."
            };
            return div()
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
                .child(div().mt_2().child(choices))
                .child(
                    div()
                        .mt_2()
                        .text_xs()
                        .text_color(theme::faint())
                        .child(status),
                )
                .into_any_element();
        }
        if !settings.model.is_empty() && self.listed_model(&settings.model).is_none() {
            choices = choices.child(pill("model-custom", settings.model.clone(), true));
        }
        choices.into_any_element()
    }
}
