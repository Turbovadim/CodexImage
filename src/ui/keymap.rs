//! Global actions, their key bindings, and the application menu.

use super::input::{self, Delete, Motion, Move, Select};
use crate::APP_NAME;
#[cfg(target_os = "macos")]
use gpui::SystemMenuType;
use gpui::{Action, App, KeyBinding, Menu, MenuItem, OsAction, actions};

actions!(
    codex_image,
    [
        Generate,
        FocusPrompt,
        OpenBoards,
        ToggleGallery,
        FitCanvas,
        ZoomIn,
        ZoomOut,
        ResetZoom,
        Escape,
        BranchHovered,
        CombineHovered,
        RegenerateHovered,
        EditHovered,
        DuplicateHovered,
        DeleteHovered,
        LightboxLeft,
        LightboxRight,
        LightboxUp,
        LightboxDown,
        ToggleCompare,
        AddAttachment,
        ChatHovered,
        OpenSettings,
        Quit,
    ]
);

pub(super) const fn platform_shortcut(
    macos: &'static str,
    non_macos: &'static str,
) -> &'static str {
    if cfg!(target_os = "macos") {
        macos
    } else {
        non_macos
    }
}

fn input_key(keystrokes: &'static str, action: impl Action) -> KeyBinding {
    KeyBinding::new(keystrokes, action, Some("CodexImageInput"))
}

/// Binds each keystroke to move by its motion, and with shift to select by it.
macro_rules! motion_keys {
    ($($keys:literal => $motion:ident),* $(,)?) => {
        [$(
            input_key($keys, Move(Motion::$motion)),
            input_key(concat!("shift-", $keys), Select(Motion::$motion)),
        )*]
    };
}

pub fn bind_keys(cx: &mut App) {
    const CANVAS_CONTEXT: &str = "CodexImage && !CodexImageInput";

    cx.bind_keys([
        KeyBinding::new("enter", Generate, None),
        KeyBinding::new("/", FocusPrompt, Some(CANVAS_CONTEXT)),
        KeyBinding::new("g", ToggleGallery, Some(CANVAS_CONTEXT)),
        KeyBinding::new("f", FitCanvas, Some(CANVAS_CONTEXT)),
        KeyBinding::new("escape", Escape, None),
        KeyBinding::new("b", BranchHovered, Some(CANVAS_CONTEXT)),
        KeyBinding::new("c", CombineHovered, Some(CANVAS_CONTEXT)),
        KeyBinding::new("r", RegenerateHovered, Some(CANVAS_CONTEXT)),
        KeyBinding::new("e", EditHovered, Some(CANVAS_CONTEXT)),
        KeyBinding::new("d", DuplicateHovered, Some(CANVAS_CONTEXT)),
        KeyBinding::new("t", ChatHovered, Some(CANVAS_CONTEXT)),
        KeyBinding::new("backspace", DeleteHovered, Some(CANVAS_CONTEXT)),
        KeyBinding::new("delete", DeleteHovered, Some(CANVAS_CONTEXT)),
        KeyBinding::new("left", LightboxLeft, Some(CANVAS_CONTEXT)),
        KeyBinding::new("right", LightboxRight, Some(CANVAS_CONTEXT)),
        KeyBinding::new("up", LightboxUp, Some(CANVAS_CONTEXT)),
        KeyBinding::new("down", LightboxDown, Some(CANVAS_CONTEXT)),
        KeyBinding::new("x", ToggleCompare, Some(CANVAS_CONTEXT)),
        input_key("shift-enter", input::InsertNewline),
        input_key("backspace", Delete(Motion::Left)),
        input_key("shift-backspace", Delete(Motion::Left)),
        input_key("delete", Delete(Motion::Right)),
    ]);
    cx.bind_keys(motion_keys![
        "left" => Left,
        "right" => Right,
        "up" => Up,
        "down" => Down,
        "home" => LineStart,
        "end" => LineEnd,
    ]);
    bind_platform_keys(cx);
}

#[cfg(target_os = "macos")]
fn bind_platform_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-k", OpenBoards, None),
        KeyBinding::new("cmd-=", ZoomIn, None),
        KeyBinding::new("cmd--", ZoomOut, None),
        KeyBinding::new("cmd-0", ResetZoom, None),
        KeyBinding::new("cmd-o", AddAttachment, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-q", Quit, None),
        input_key("ctrl-h", Delete(Motion::Left)),
        input_key("ctrl-d", Delete(Motion::Right)),
        input_key("alt-backspace", Delete(Motion::WordLeft)),
        input_key("ctrl-w", Delete(Motion::WordLeft)),
        input_key("alt-delete", Delete(Motion::WordRight)),
        input_key("cmd-backspace", Delete(Motion::LineStart)),
        input_key("cmd-delete", Delete(Motion::LineEnd)),
        input_key("ctrl-k", Delete(Motion::LineEnd)),
        input_key("cmd-a", input::SelectAll),
        input_key("cmd-v", input::Paste),
        input_key("cmd-c", input::Copy),
        input_key("cmd-x", input::Cut),
        input_key("cmd-z", input::Undo),
        input_key("cmd-shift-z", input::Redo),
        input_key("ctrl-cmd-space", input::ShowCharacterPalette),
    ]);
    cx.bind_keys(motion_keys![
        "alt-left" => WordLeft,
        "alt-right" => WordRight,
        "cmd-left" => LineStart,
        "cmd-right" => LineEnd,
        "cmd-up" => DocumentStart,
        "cmd-down" => DocumentEnd,
        "cmd-home" => DocumentStart,
        "cmd-end" => DocumentEnd,
        "ctrl-a" => LineStart,
        "ctrl-e" => LineEnd,
        "ctrl-b" => Left,
        "ctrl-f" => Right,
        "ctrl-p" => Up,
        "ctrl-n" => Down,
    ]);
}

#[cfg(not(target_os = "macos"))]
fn bind_platform_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-k", OpenBoards, None),
        KeyBinding::new("ctrl-=", ZoomIn, None),
        KeyBinding::new("ctrl--", ZoomOut, None),
        KeyBinding::new("ctrl-0", ResetZoom, None),
        KeyBinding::new("ctrl-o", AddAttachment, None),
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("ctrl-q", Quit, None),
        input_key("ctrl-backspace", Delete(Motion::WordLeft)),
        input_key("ctrl-delete", Delete(Motion::WordRight)),
        input_key("ctrl-a", input::SelectAll),
        input_key("ctrl-v", input::Paste),
        input_key("ctrl-c", input::Copy),
        input_key("ctrl-x", input::Cut),
        input_key("ctrl-z", input::Undo),
        input_key("ctrl-y", input::Redo),
        input_key("ctrl-shift-z", input::Redo),
    ]);
    cx.bind_keys(motion_keys![
        "ctrl-left" => WordLeft,
        "ctrl-right" => WordRight,
        "ctrl-home" => DocumentStart,
        "ctrl-end" => DocumentEnd,
    ]);
}

pub fn configure_menus(cx: &mut App) {
    #[cfg(target_os = "macos")]
    let leading = [
        Menu::new(APP_NAME).items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(format!("Quit {APP_NAME}"), Quit),
        ]),
        Menu::new("File").items([MenuItem::action("Attach Images…", AddAttachment)]),
    ];
    #[cfg(not(target_os = "macos"))]
    let leading = [Menu::new("File").items([
        MenuItem::action("Attach Images…", AddAttachment),
        MenuItem::action("Settings…", OpenSettings),
        MenuItem::separator(),
        MenuItem::action(format!("Exit {APP_NAME}"), Quit),
    ])];

    cx.set_menus(leading.into_iter().chain([
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
            MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
            MenuItem::separator(),
            MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Boards", OpenBoards),
            MenuItem::action("Gallery", ToggleGallery),
            MenuItem::action("Fit Canvas", FitCanvas),
            MenuItem::action("Zoom In", ZoomIn),
            MenuItem::action("Zoom Out", ZoomOut),
            MenuItem::action("Actual Size", ResetZoom),
        ]),
    ]));
}
