//! Global actions, their key bindings, and the application menu.

use super::input::{
    Backspace as InputBackspace, Copy as InputCopy, Cut as InputCut, Delete as InputDelete,
    DeleteWordBackward as InputDeleteWordBackward, DeleteWordForward as InputDeleteWordForward,
    DocumentEnd as InputDocumentEnd, DocumentStart as InputDocumentStart, Down as InputDown,
    End as InputEnd, Home as InputHome, InsertNewline, Left as InputLeft, Paste as InputPaste,
    Redo as InputRedo, Right as InputRight, SelectAll as InputSelectAll,
    SelectDocumentEnd as InputSelectDocumentEnd, SelectDocumentStart as InputSelectDocumentStart,
    SelectDown as InputSelectDown, SelectEnd as InputSelectEnd, SelectHome as InputSelectHome,
    SelectLeft as InputSelectLeft, SelectRight as InputSelectRight, SelectUp as InputSelectUp,
    SelectWordLeft as InputSelectWordLeft, SelectWordRight as InputSelectWordRight,
    Undo as InputUndo, Up as InputUp, WordLeft as InputWordLeft, WordRight as InputWordRight,
};
#[cfg(target_os = "macos")]
use super::input::{
    DeleteToLineEnd as InputDeleteToLineEnd, DeleteToLineStart as InputDeleteToLineStart,
    ShowCharacterPalette as InputShowCharacterPalette,
};
use crate::APP_NAME;
#[cfg(target_os = "macos")]
use gpui::SystemMenuType;
use gpui::{App, KeyBinding, Menu, MenuItem, OsAction, actions};

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
        RegenerateHovered,
        EditHovered,
        DuplicateHovered,
        DeleteHovered,
        LightboxLeft,
        LightboxRight,
        LightboxUp,
        LightboxDown,
        AddAttachment,
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

pub fn bind_keys(cx: &mut App) {
    const CANVAS_CONTEXT: &str = "CodexImage && !CodexImageInput";

    cx.bind_keys([
        KeyBinding::new("enter", Generate, None),
        KeyBinding::new("shift-enter", InsertNewline, Some("CodexImageInput")),
        KeyBinding::new("/", FocusPrompt, Some(CANVAS_CONTEXT)),
        KeyBinding::new("g", ToggleGallery, Some(CANVAS_CONTEXT)),
        KeyBinding::new("f", FitCanvas, Some(CANVAS_CONTEXT)),
        KeyBinding::new("escape", Escape, None),
        KeyBinding::new("b", BranchHovered, Some(CANVAS_CONTEXT)),
        KeyBinding::new("r", RegenerateHovered, Some(CANVAS_CONTEXT)),
        KeyBinding::new("e", EditHovered, Some(CANVAS_CONTEXT)),
        KeyBinding::new("d", DuplicateHovered, Some(CANVAS_CONTEXT)),
        KeyBinding::new("backspace", DeleteHovered, Some(CANVAS_CONTEXT)),
        KeyBinding::new("delete", DeleteHovered, Some(CANVAS_CONTEXT)),
        KeyBinding::new("left", LightboxLeft, Some(CANVAS_CONTEXT)),
        KeyBinding::new("right", LightboxRight, Some(CANVAS_CONTEXT)),
        KeyBinding::new("up", LightboxUp, Some(CANVAS_CONTEXT)),
        KeyBinding::new("down", LightboxDown, Some(CANVAS_CONTEXT)),
        KeyBinding::new("backspace", InputBackspace, Some("CodexImageInput")),
        KeyBinding::new("shift-backspace", InputBackspace, Some("CodexImageInput")),
        KeyBinding::new("delete", InputDelete, Some("CodexImageInput")),
        KeyBinding::new("left", InputLeft, Some("CodexImageInput")),
        KeyBinding::new("right", InputRight, Some("CodexImageInput")),
        KeyBinding::new("up", InputUp, Some("CodexImageInput")),
        KeyBinding::new("down", InputDown, Some("CodexImageInput")),
        KeyBinding::new("shift-left", InputSelectLeft, Some("CodexImageInput")),
        KeyBinding::new("shift-right", InputSelectRight, Some("CodexImageInput")),
        KeyBinding::new("shift-up", InputSelectUp, Some("CodexImageInput")),
        KeyBinding::new("shift-down", InputSelectDown, Some("CodexImageInput")),
        KeyBinding::new("shift-home", InputSelectHome, Some("CodexImageInput")),
        KeyBinding::new("shift-end", InputSelectEnd, Some("CodexImageInput")),
        KeyBinding::new("home", InputHome, Some("CodexImageInput")),
        KeyBinding::new("end", InputEnd, Some("CodexImageInput")),
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
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("ctrl-h", InputBackspace, Some("CodexImageInput")),
        KeyBinding::new("ctrl-d", InputDelete, Some("CodexImageInput")),
        KeyBinding::new("alt-left", InputWordLeft, Some("CodexImageInput")),
        KeyBinding::new("alt-right", InputWordRight, Some("CodexImageInput")),
        KeyBinding::new(
            "alt-shift-left",
            InputSelectWordLeft,
            Some("CodexImageInput"),
        ),
        KeyBinding::new(
            "alt-shift-right",
            InputSelectWordRight,
            Some("CodexImageInput"),
        ),
        KeyBinding::new("cmd-left", InputHome, Some("CodexImageInput")),
        KeyBinding::new("cmd-right", InputEnd, Some("CodexImageInput")),
        KeyBinding::new("cmd-shift-left", InputSelectHome, Some("CodexImageInput")),
        KeyBinding::new("cmd-shift-right", InputSelectEnd, Some("CodexImageInput")),
        KeyBinding::new("cmd-up", InputDocumentStart, Some("CodexImageInput")),
        KeyBinding::new("cmd-down", InputDocumentEnd, Some("CodexImageInput")),
        KeyBinding::new(
            "cmd-shift-up",
            InputSelectDocumentStart,
            Some("CodexImageInput"),
        ),
        KeyBinding::new(
            "cmd-shift-down",
            InputSelectDocumentEnd,
            Some("CodexImageInput"),
        ),
        KeyBinding::new(
            "alt-backspace",
            InputDeleteWordBackward,
            Some("CodexImageInput"),
        ),
        KeyBinding::new("ctrl-w", InputDeleteWordBackward, Some("CodexImageInput")),
        KeyBinding::new(
            "alt-delete",
            InputDeleteWordForward,
            Some("CodexImageInput"),
        ),
        KeyBinding::new(
            "cmd-backspace",
            InputDeleteToLineStart,
            Some("CodexImageInput"),
        ),
        KeyBinding::new("cmd-delete", InputDeleteToLineEnd, Some("CodexImageInput")),
        KeyBinding::new("ctrl-k", InputDeleteToLineEnd, Some("CodexImageInput")),
        KeyBinding::new("cmd-a", InputSelectAll, Some("CodexImageInput")),
        KeyBinding::new("cmd-v", InputPaste, Some("CodexImageInput")),
        KeyBinding::new("cmd-c", InputCopy, Some("CodexImageInput")),
        KeyBinding::new("cmd-x", InputCut, Some("CodexImageInput")),
        KeyBinding::new("cmd-z", InputUndo, Some("CodexImageInput")),
        KeyBinding::new("cmd-shift-z", InputRedo, Some("CodexImageInput")),
        KeyBinding::new(
            "ctrl-cmd-space",
            InputShowCharacterPalette,
            Some("CodexImageInput"),
        ),
        KeyBinding::new("ctrl-a", InputHome, Some("CodexImageInput")),
        KeyBinding::new("ctrl-e", InputEnd, Some("CodexImageInput")),
        KeyBinding::new("ctrl-b", InputLeft, Some("CodexImageInput")),
        KeyBinding::new("ctrl-f", InputRight, Some("CodexImageInput")),
        KeyBinding::new("ctrl-p", InputUp, Some("CodexImageInput")),
        KeyBinding::new("ctrl-n", InputDown, Some("CodexImageInput")),
        KeyBinding::new("cmd-home", InputDocumentStart, Some("CodexImageInput")),
        KeyBinding::new("cmd-end", InputDocumentEnd, Some("CodexImageInput")),
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
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("ctrl-left", InputWordLeft, Some("CodexImageInput")),
        KeyBinding::new("ctrl-right", InputWordRight, Some("CodexImageInput")),
        KeyBinding::new(
            "ctrl-shift-left",
            InputSelectWordLeft,
            Some("CodexImageInput"),
        ),
        KeyBinding::new(
            "ctrl-shift-right",
            InputSelectWordRight,
            Some("CodexImageInput"),
        ),
        KeyBinding::new("ctrl-home", InputDocumentStart, Some("CodexImageInput")),
        KeyBinding::new("ctrl-end", InputDocumentEnd, Some("CodexImageInput")),
        KeyBinding::new(
            "ctrl-shift-home",
            InputSelectDocumentStart,
            Some("CodexImageInput"),
        ),
        KeyBinding::new(
            "ctrl-shift-end",
            InputSelectDocumentEnd,
            Some("CodexImageInput"),
        ),
        KeyBinding::new(
            "ctrl-backspace",
            InputDeleteWordBackward,
            Some("CodexImageInput"),
        ),
        KeyBinding::new(
            "ctrl-delete",
            InputDeleteWordForward,
            Some("CodexImageInput"),
        ),
        KeyBinding::new("ctrl-a", InputSelectAll, Some("CodexImageInput")),
        KeyBinding::new("ctrl-v", InputPaste, Some("CodexImageInput")),
        KeyBinding::new("ctrl-c", InputCopy, Some("CodexImageInput")),
        KeyBinding::new("ctrl-x", InputCut, Some("CodexImageInput")),
        KeyBinding::new("ctrl-z", InputUndo, Some("CodexImageInput")),
        KeyBinding::new("ctrl-y", InputRedo, Some("CodexImageInput")),
        KeyBinding::new("ctrl-shift-z", InputRedo, Some("CodexImageInput")),
    ]);
}

pub fn configure_menus(cx: &mut App) {
    #[cfg(target_os = "macos")]
    cx.set_menus([
        Menu::new(APP_NAME).items([
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(format!("Quit {APP_NAME}"), Quit),
        ]),
        Menu::new("File").items([MenuItem::action("Attach Images…", AddAttachment)]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", InputUndo, OsAction::Undo),
            MenuItem::os_action("Redo", InputRedo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", InputCut, OsAction::Cut),
            MenuItem::os_action("Copy", InputCopy, OsAction::Copy),
            MenuItem::os_action("Paste", InputPaste, OsAction::Paste),
            MenuItem::separator(),
            MenuItem::os_action("Select All", InputSelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Boards", OpenBoards),
            MenuItem::action("Gallery", ToggleGallery),
            MenuItem::action("Fit Canvas", FitCanvas),
            MenuItem::action("Zoom In", ZoomIn),
            MenuItem::action("Zoom Out", ZoomOut),
            MenuItem::action("Actual Size", ResetZoom),
        ]),
    ]);

    #[cfg(not(target_os = "macos"))]
    cx.set_menus([
        Menu::new("File").items([
            MenuItem::action("Attach Images…", AddAttachment),
            MenuItem::separator(),
            MenuItem::action(format!("Exit {APP_NAME}"), Quit),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", InputUndo, OsAction::Undo),
            MenuItem::os_action("Redo", InputRedo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", InputCut, OsAction::Cut),
            MenuItem::os_action("Copy", InputCopy, OsAction::Copy),
            MenuItem::os_action("Paste", InputPaste, OsAction::Paste),
            MenuItem::separator(),
            MenuItem::os_action("Select All", InputSelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Boards", OpenBoards),
            MenuItem::action("Gallery", ToggleGallery),
            MenuItem::action("Fit Canvas", FitCanvas),
            MenuItem::action("Zoom In", ZoomIn),
            MenuItem::action("Zoom Out", ZoomOut),
            MenuItem::action("Actual Size", ResetZoom),
        ]),
    ]);
}
