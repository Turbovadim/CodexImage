//! The frame every modal dialog shares: scrim, panel, header and footer, plus
//! the buttons, fields, and segmented controls that go inside one.

use super::app::AppView;
use super::icons::{Icon, icon};
use super::input::TextInput;
use super::theme;
use super::tooltip::tip_with_shortcut;
use gpui::{
    Animation, AnimationExt, AnyElement, App, BoxShadow, Context, Div, Entity, Focusable,
    FontWeight, Role, SharedString, Stateful, Window, div, ease_out_quint, prelude::*, px,
};
use std::time::Duration;

const ENTER: Duration = Duration::from_millis(160);

#[derive(Clone, Copy)]
pub(super) enum ButtonKind {
    Primary,
    Secondary,
    Danger,
}

/// The dimmed backdrop a dialog sits on. Callers add their own click or drop
/// handling, then hand it to [`present`] together with the panel.
pub(super) fn scrim(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .inset_0()
        .p_4()
        .bg(gpui::black().opacity(0.6))
        .flex()
        .items_center()
        .justify_center()
        .occlude()
}

pub(super) fn panel(id: &'static str, label: impl Into<SharedString>, width: f32) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Dialog)
        .aria_label(label)
        .relative()
        .w(px(width))
        .max_w_full()
        .max_h_full()
        .flex()
        .flex_col()
        .rounded(px(14.))
        .border_1()
        .border_color(theme::line())
        .bg(theme::raised())
        .shadow(vec![
            BoxShadow::new(px(0.), px(24.), gpui::black().opacity(0.55)).blur_radius(px(64.)),
        ])
        .occlude()
}

/// Puts `panel` on `scrim`: the backdrop fades in while the panel settles up
/// into place.
pub(super) fn present(scrim: Stateful<Div>, panel: Stateful<Div>) -> AnyElement {
    scrim
        .child(panel.with_animation(
            "dialog-rise",
            Animation::new(ENTER).with_easing(ease_out_quint()),
            |panel, delta| panel.top(px((1. - delta) * 10.)),
        ))
        .with_animation("dialog-fade", Animation::new(ENTER), |scrim, delta| {
            scrim.opacity(delta)
        })
        .into_any_element()
}

pub(super) fn title(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::ink())
        .child(text.into())
}

/// The dialog's top bar: `lead` on the left, an optional action, and the
/// close button every dialog ends with.
pub(super) fn header(
    lead: impl IntoElement,
    action: Option<AnyElement>,
    cx: &mut Context<AppView>,
) -> Div {
    div()
        .flex_none()
        .h(px(46.))
        .pl_4()
        .pr_2()
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(theme::line())
        .child(div().flex_1().min_w_0().flex().items_center().child(lead))
        .children(action)
        .child(
            div()
                .id("dialog-close")
                .role(Role::Button)
                .aria_label("Close")
                .size(px(28.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded_md()
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover()))
                .active(|style| style.bg(theme::line()))
                .tooltip(tip_with_shortcut("Close", Some("Esc")))
                .child(icon(Icon::X, 14.).text_color(theme::dim()))
                .on_click(cx.listener(|this, _, window, cx| this.close_overlay(window, cx))),
        )
}

/// The dialog's bottom bar. Children line up on the right; a [`hint`] takes
/// the remaining space on the left.
pub(super) fn footer() -> Div {
    div()
        .flex_none()
        .px_4()
        .py_3()
        .flex()
        .items_center()
        .justify_end()
        .gap_2()
        .border_t_1()
        .border_color(theme::line())
}

pub(super) fn hint(text: impl Into<SharedString>) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .truncate()
        .text_xs()
        .text_color(theme::faint())
        .child(text.into())
}

pub(super) fn button(
    id: &'static str,
    label: impl Into<SharedString>,
    kind: ButtonKind,
) -> Stateful<Div> {
    let label = label.into();
    let button = div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .h(px(32.))
        .px_3()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_lg()
        .border_1()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .child(label);
    match kind {
        ButtonKind::Primary => button
            .border_color(theme::accent_strong())
            .bg(theme::accent_strong())
            .text_color(gpui::white())
            .hover(|style| style.bg(theme::accent()).border_color(theme::accent()))
            .active(|style| style.bg(theme::accent_strong())),
        ButtonKind::Secondary => button
            .border_color(theme::line())
            .bg(theme::hover().opacity(0.6))
            .text_color(theme::dim())
            .hover(|style| {
                style
                    .bg(theme::hover())
                    .border_color(theme::faint().opacity(0.8))
                    .text_color(theme::ink())
            })
            .active(|style| style.bg(theme::line())),
        ButtonKind::Danger => button
            .border_color(theme::danger().opacity(0.3))
            .bg(theme::danger().opacity(0.14))
            .text_color(theme::danger())
            .hover(|style| style.bg(theme::danger().opacity(0.24)))
            .active(|style| style.bg(theme::danger().opacity(0.32))),
    }
}

/// A text input in its box, outlined in the accent colour while focused.
pub(super) fn field(input: &Entity<TextInput>, window: &Window, cx: &App) -> Div {
    let focused = input.focus_handle(cx).is_focused(window);
    div()
        .rounded_lg()
        .border_1()
        .border_color(if focused {
            theme::accent().opacity(0.55)
        } else {
            theme::line()
        })
        .bg(theme::background())
        .px_3()
        .py_1p5()
        .child(input.clone())
}

/// The track of a segmented control; fill it with [`segment`]s.
pub(super) fn segmented() -> Div {
    div()
        .flex()
        .flex_wrap()
        .gap(px(2.))
        .p(px(2.))
        .rounded_lg()
        .border_1()
        .border_color(theme::line())
        .bg(theme::background())
}

pub(super) fn segment(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    active: bool,
) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(id.into())
        .role(Role::Button)
        .aria_label(label.clone())
        .h(px(26.))
        .min_w(px(32.))
        .px(px(10.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .when(active, |segment| {
            segment.bg(theme::line()).text_color(theme::ink())
        })
        .when(!active, |segment| {
            segment
                .text_color(theme::dim())
                .hover(|style| style.bg(theme::hover()).text_color(theme::ink()))
        })
        .child(label)
}
