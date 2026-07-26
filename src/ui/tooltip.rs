//! A minimal hover tooltip, used to explain the icon-sized controls that would
//! otherwise only be discoverable by trial and error.

use super::theme;
use gpui::{AnyView, App, Context, FontWeight, Render, SharedString, Window, div, prelude::*, px};

pub(super) struct Tooltip {
    text: SharedString,
    shortcut: Option<SharedString>,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut row = div()
            .flex()
            .items_center()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(theme::line())
            .bg(theme::raised())
            .px_2()
            .py_1()
            .text_xs()
            .text_color(theme::ink())
            .child(self.text.clone());
        if let Some(shortcut) = &self.shortcut {
            row = row.child(
                div()
                    .rounded_md()
                    .bg(theme::background())
                    .px_1()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::dim())
                    .child(shortcut.clone()),
            );
        }
        div().pt(px(2.)).child(row)
    }
}

/// Builds a `.tooltip(…)` callback showing `text`.
pub(super) fn tip(
    text: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    tip_with_shortcut(text, None::<SharedString>)
}

/// Builds a `.tooltip(…)` callback showing `text` next to a keyboard shortcut.
pub(super) fn tip_with_shortcut(
    text: impl Into<SharedString>,
    shortcut: Option<impl Into<SharedString>>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    let shortcut = shortcut.map(Into::into);
    move |_, cx| {
        let (text, shortcut) = (text.clone(), shortcut.clone());
        cx.new(|_| Tooltip { text, shortcut }).into()
    }
}
