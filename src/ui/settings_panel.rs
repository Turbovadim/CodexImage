//! The settings overlay: which model this app asks Codex for, how hard it
//! thinks, and how much of a card's chain travels with a generation.

use super::app::{AppView, Overlay};
use super::dialog::{
    ButtonKind, button, field, footer, header, hint, panel, present, scrim, segment, segmented,
    title,
};
use super::icons::{Icon, icon};
use super::theme;
use super::tooltip::tip;
use crate::generation::CodexModel;
use crate::settings::{MAX_LINEAGE_REFS, SettingsData};
use gpui::{
    AnyElement, Context, Div, Focusable, Role, SharedString, Stateful, Window, div, prelude::*, px,
};
use std::sync::Arc;

const LABEL_WIDTH: f32 = 132.;
const MODEL_ROW_HEIGHT: f32 = 30.;

/// One setting: its name on the left, explained by a tooltip, and its control.
fn row(label: &'static str, explanation: &'static str, control: impl IntoElement) -> Div {
    div()
        .flex()
        .items_start()
        .gap_4()
        .child(
            div()
                .id(label)
                .w(px(LABEL_WIDTH))
                .h(px(32.))
                .flex()
                .flex_none()
                .items_center()
                .text_sm()
                .text_color(theme::dim())
                .tooltip(tip(explanation))
                .child(label),
        )
        .child(div().flex_1().min_w_0().flex().child(control))
}

/// A line of the model list: a check mark on the chosen one, the name, and
/// whatever Codex says about it.
fn model_row(
    id: impl Into<SharedString>,
    name: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    active: bool,
) -> Stateful<Div> {
    let name = name.into();
    div()
        .id(id.into())
        .role(Role::Button)
        .aria_label(name.clone())
        .h(px(MODEL_ROW_HEIGHT))
        .px_2()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .rounded_md()
        .cursor_pointer()
        .when(active, |row| row.bg(theme::hover()))
        .hover(|style| style.bg(theme::hover()))
        .child(icon(Icon::Check, 14.).text_color(if active {
            theme::accent()
        } else {
            gpui::transparent_black()
        }))
        .child(
            div()
                .flex_none()
                .text_sm()
                .text_color(if active { theme::ink() } else { theme::dim() })
                .child(name),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(theme::faint())
                .child(detail.into()),
        )
}

impl AppView {
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.engine.settings().get().model;
        self.modal_input.update(cx, |input, cx| {
            input.set_mode(super::input::TextInputMode::SingleLine, cx);
            input.set_placeholder("Model id, blank for the CLI default…", cx);
            input.set_content(model, cx);
            input.select_content(cx);
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

    pub(super) fn render_settings(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let settings = self.engine.settings().get();
        let active = self.listed_model(&settings.model);
        let mut efforts = segmented().child(
            segment(
                "effort-default",
                match active.and_then(|model| model.default_reasoning_effort.as_deref()) {
                    Some(effort) => format!("Default · {effort}"),
                    None => "Default".to_owned(),
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
                segment(
                    format!("effort-{effort}"),
                    effort.clone(),
                    settings.reasoning_effort == effort,
                )
                .when(!option.description.is_empty(), |segment| {
                    segment.tooltip(tip(option.description.clone()))
                })
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
            efforts = efforts.child(segment(
                "effort-custom",
                settings.reasoning_effort.clone(),
                true,
            ));
        }
        let lineage = settings.lineage_refs;
        let references = segmented().children((0..=MAX_LINEAGE_REFS).map(|count| {
            segment(
                format!("lineage-{count}"),
                if count == 0 {
                    "Off".to_owned()
                } else {
                    count.to_string()
                },
                lineage == count,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.change_settings(|settings| settings.lineage_refs = count, cx)
            }))
        }));
        let panel = panel("settings-panel", "Codex settings", 620.)
            .child(header(title("Codex settings"), None, cx))
            .child(
                div()
                    .min_h_0()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(row(
                        "Model",
                        "Passed as codex exec -m",
                        self.render_model_choices(&settings, window, cx),
                    ))
                    .child(row(
                        "Reasoning effort",
                        "Passed as model_reasoning_effort",
                        efforts,
                    ))
                    .child(row(
                        "Chain references",
                        "Earlier images from a card's own chain, sent along as identity reference",
                        references,
                    )),
            )
            .child(
                footer().child(hint("Applies to this app only")).child(
                    button("settings-save", "Done", ButtonKind::Primary)
                        .on_click(cx.listener(|this, _, window, cx| this.save_model(window, cx))),
                ),
            );
        present(
            scrim("settings-overlay")
                .on_click(cx.listener(|this, _, window, cx| this.close_overlay(window, cx))),
            panel,
        )
    }

    /// One row per model Codex lists. Until it lists any, a free-text id
    /// field stands in so a missing or signed-out CLI never blocks the choice.
    fn render_model_choices(
        &self,
        settings: &SettingsData,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.codex_models.is_empty() {
            let status = if self.codex_models_task.is_some() {
                "Loading models from Codex…"
            } else {
                "Codex did not list any models. Type an id instead."
            };
            return div()
                .w_full()
                .child(field(&self.modal_input, window, cx))
                .child(
                    div()
                        .mt_2()
                        .text_xs()
                        .text_color(theme::faint())
                        .child(status),
                )
                .into_any_element();
        }
        // The list scrolls rather than pushing the dialog past a short window.
        let max_height =
            (f32::from(window.viewport_size().height) - 330.).max(4. * MODEL_ROW_HEIGHT);
        let default_model = self
            .codex_models
            .iter()
            .find(|model| model.is_default)
            .map(|model| model.label().to_owned())
            .unwrap_or_default();
        let mut choices = div()
            .id("model-list")
            .w_full()
            .max_h(px(max_height))
            .overflow_y_scroll()
            .px_1()
            .py(px(2.))
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(theme::line())
            .bg(theme::background())
            .child(
                model_row(
                    "model-default",
                    "CLI default",
                    default_model,
                    settings.model.is_empty(),
                )
                .on_click(cx.listener(|this, _, _, cx| this.pick_model(String::new(), cx))),
            );
        for model in self.codex_models.iter() {
            let id = model.model.clone();
            choices = choices.child(
                model_row(
                    format!("model-{id}"),
                    model.label().to_owned(),
                    model.description.clone(),
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
        if !settings.model.is_empty() && self.listed_model(&settings.model).is_none() {
            choices = choices.child(model_row(
                "model-custom",
                settings.model.clone(),
                "Not listed by Codex",
                true,
            ));
        }
        choices.into_any_element()
    }
}
