use super::input::{
    Backspace as InputBackspace, Copy as InputCopy, Cut as InputCut, Delete as InputDelete,
    DeleteToLineEnd as InputDeleteToLineEnd, DeleteToLineStart as InputDeleteToLineStart,
    DeleteWordBackward as InputDeleteWordBackward, DeleteWordForward as InputDeleteWordForward,
    DocumentEnd as InputDocumentEnd, DocumentStart as InputDocumentStart, Down as InputDown,
    End as InputEnd, Home as InputHome, InsertNewline, Left as InputLeft, Paste as InputPaste,
    Redo as InputRedo, Right as InputRight, SelectAll as InputSelectAll,
    SelectDocumentEnd as InputSelectDocumentEnd, SelectDocumentStart as InputSelectDocumentStart,
    SelectDown as InputSelectDown, SelectEnd as InputSelectEnd, SelectHome as InputSelectHome,
    SelectLeft as InputSelectLeft, SelectRight as InputSelectRight, SelectUp as InputSelectUp,
    SelectWordLeft as InputSelectWordLeft, SelectWordRight as InputSelectWordRight,
    ShowCharacterPalette as InputShowCharacterPalette, TextInput, TextInputEvent, TextInputMode,
    Undo as InputUndo, Up as InputUp, WordLeft as InputWordLeft, WordRight as InputWordRight,
};
use super::theme;
use crate::APP_NAME;
use crate::generation::GenerationEngine;
use crate::layout::{CARD_WIDTH, Position, compute_layout};
use crate::model::{Board, BoardNode, NewNodesRequest, NodeStatus, StopReason};
use crate::storage::{RepositoryEvent, THUMBNAIL_MAX_DIMENSION, create_thumbnail, now_ms};
use anyhow::{Context as _, Result};
use gpui::{
    AnyElement, App, AppContext, BorderStyle, Bounds, ClipboardItem, ContentMask, Context, Entity,
    ExternalPaths, FocusHandle, Focusable, FontWeight, Image, ImageFormat, ImgResourceLoader,
    KeyBinding, Menu, MenuItem, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ObjectFit, OsAction, PathBuilder, PathPromptOptions, PinchEvent, Pixels, Point, Render,
    Resource, Role, ScrollWheelEvent, SharedString, StyledImage, SystemMenuType, TextAlign,
    TextRun, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, canvas, div, fill, img,
    point, prelude::*, px, quad, size,
};
use gpui_platform::application;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

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

const ASPECTS: &[&str] = &["auto", "1:1", "16:9", "9:16", "4:3", "3:4"];
const LIGHTBOX_MIN_ZOOM: f32 = 1.;
const LIGHTBOX_MAX_ZOOM: f32 = 8.;
const NODE_TOOLBAR_HEIGHT: f32 = 36.;
const SAMPLES: &[&str] = &[
    "A cozy cabin in a snowy forest at dusk, warm light in the windows",
    "Isometric illustration of a tiny home office, pastel palette",
    "Logo concept for a coffee brand called \"Ember\", minimal, flat",
    "Studio photo of a perfume bottle on black marble, dramatic lighting",
];

#[derive(Clone)]
struct ComposerTarget {
    node_id: String,
    prompt: String,
    source_image: Option<String>,
}

enum Overlay {
    None,
    Boards,
    Gallery,
    Lightbox(Lightbox),
    EditNode(String),
    RenameBoard(String),
    QuitConfirm,
}

struct Lightbox {
    node_id: String,
    image: String,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
    pending: Option<LightboxLocation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LightboxLocation {
    node_id: String,
    image: String,
}

impl Lightbox {
    fn displayed_location(&self) -> LightboxLocation {
        LightboxLocation {
            node_id: self.node_id.clone(),
            image: self.image.clone(),
        }
    }

    fn navigation_origin(&self) -> LightboxLocation {
        self.pending
            .clone()
            .unwrap_or_else(|| self.displayed_location())
    }

    fn request(&mut self, target: LightboxLocation) {
        self.pending = (target != self.displayed_location()).then_some(target);
    }

    fn commit_pending(&mut self, expected: &LightboxLocation) -> bool {
        if self.pending.as_ref() != Some(expected) {
            return false;
        }
        let target = self
            .pending
            .take()
            .expect("pending target was just checked");
        self.node_id = target.node_id;
        self.image = target.image;
        self.reset_view();
        true
    }

    fn reset_view(&mut self) {
        self.zoom = LIGHTBOX_MIN_ZOOM;
        self.pan_x = 0.;
        self.pan_y = 0.;
    }

    fn zoom_at(
        &mut self,
        factor: f32,
        focal: Point<Pixels>,
        viewport_width: f32,
        viewport_height: f32,
        image_ratio: f32,
    ) {
        if !factor.is_finite() || factor <= 0. {
            return;
        }
        let previous_zoom = self.zoom;
        let next_zoom = (previous_zoom * factor).clamp(LIGHTBOX_MIN_ZOOM, LIGHTBOX_MAX_ZOOM);
        let zoom_ratio = next_zoom / previous_zoom;
        let focal_x = f32::from(focal.x) - viewport_width / 2.;
        let focal_y = f32::from(focal.y) - viewport_height / 2.;
        self.pan_x += (focal_x - self.pan_x) * (1. - zoom_ratio);
        self.pan_y += (focal_y - self.pan_y) * (1. - zoom_ratio);
        self.zoom = next_zoom;
        if self.zoom <= LIGHTBOX_MIN_ZOOM {
            self.reset_view();
        } else {
            self.clamp_pan(viewport_width, viewport_height, image_ratio);
        }
    }

    fn pan_to(
        &mut self,
        pan_x: f32,
        pan_y: f32,
        viewport_width: f32,
        viewport_height: f32,
        image_ratio: f32,
    ) {
        self.pan_x = pan_x;
        self.pan_y = pan_y;
        self.clamp_pan(viewport_width, viewport_height, image_ratio);
    }

    fn clamped_pan(
        &self,
        viewport_width: f32,
        viewport_height: f32,
        image_ratio: f32,
    ) -> (f32, f32) {
        let (fit_width, fit_height) = fitted_image_size(
            viewport_width,
            viewport_height,
            normalized_image_ratio(image_ratio),
        );
        let max_x = ((fit_width * self.zoom - viewport_width) / 2.).max(0.);
        let max_y = ((fit_height * self.zoom - viewport_height) / 2.).max(0.);
        (
            self.pan_x.clamp(-max_x, max_x),
            self.pan_y.clamp(-max_y, max_y),
        )
    }

    fn clamp_pan(&mut self, viewport_width: f32, viewport_height: f32, image_ratio: f32) {
        (self.pan_x, self.pan_y) = self.clamped_pan(viewport_width, viewport_height, image_ratio);
    }
}

fn normalized_image_ratio(image_ratio: f32) -> f32 {
    if image_ratio.is_finite() && image_ratio > 0. {
        image_ratio
    } else {
        1.
    }
}

fn fitted_image_size(viewport_width: f32, viewport_height: f32, image_ratio: f32) -> (f32, f32) {
    let viewport_width = viewport_width.max(0.);
    let viewport_height = viewport_height.max(0.);
    if viewport_width == 0. || viewport_height == 0. {
        return (0., 0.);
    }
    if viewport_width / viewport_height > image_ratio {
        (viewport_height * image_ratio, viewport_height)
    } else {
        (viewport_width, viewport_width / image_ratio)
    }
}

struct ImageAsset {
    original: PathBuf,
    thumbnail: PathBuf,
}

#[derive(Clone)]
struct CanvasImageAsset {
    original: Arc<Path>,
    thumbnail: Arc<Path>,
}

struct CanvasImage {
    url: String,
    asset: CanvasImageAsset,
}

struct CanvasNode {
    node: BoardNode,
    prompt_lines: Vec<SharedString>,
    collapsed_prompt_lines: Vec<SharedString>,
    output_layout: OutputLayout,
    displayed_images: Vec<CanvasImage>,
    attachment_images: Vec<CanvasImageAsset>,
    date: SharedString,
    done_footer: SharedString,
    status_message: SharedString,
    scene: CardScene,
    sprite_images: Vec<Arc<Image>>,
    last_ready_sprite_tier: AtomicU8,
}

#[derive(Clone, Copy)]
struct CardRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl CardRect {
    fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

#[derive(Clone, Copy)]
enum CardColor {
    Transparent,
    Background82,
    Raised,
    Hover,
    Line,
    Ink,
    Ink90,
    Dim,
    Faint,
    Accent,
    Accent45,
    Danger,
}

impl CardColor {
    fn hsla(self) -> gpui::Hsla {
        match self {
            Self::Transparent => gpui::transparent_black(),
            Self::Background82 => theme::background().opacity(0.82),
            Self::Raised => theme::raised(),
            Self::Hover => theme::hover(),
            Self::Line => theme::line(),
            Self::Ink => theme::ink(),
            Self::Ink90 => theme::ink().opacity(0.9),
            Self::Dim => theme::dim(),
            Self::Faint => theme::faint(),
            Self::Accent => theme::accent(),
            Self::Accent45 => theme::accent().opacity(0.45),
            Self::Danger => theme::danger(),
        }
    }

    fn svg(self) -> (&'static str, f32) {
        match self {
            Self::Transparent => ("#000000", 0.),
            Self::Background82 => ("#0d0e12", 0.82),
            Self::Raised => ("#14161c", 1.),
            Self::Hover => ("#1b1e26", 1.),
            Self::Line => ("#262a35", 1.),
            Self::Ink => ("#e8eaf0", 1.),
            Self::Ink90 => ("#e8eaf0", 0.9),
            Self::Dim => ("#8b90a0", 1.),
            Self::Faint => ("#5a5f6e", 1.),
            Self::Accent => ("#7c8cff", 1.),
            Self::Accent45 => ("#7c8cff", 0.45),
            Self::Danger => ("#ff6b6b", 1.),
        }
    }
}

#[derive(Clone, Copy)]
enum CardImageFit {
    Contain,
    Cover,
}

enum CardPrimitive {
    Quad {
        bounds: CardRect,
        radius: f32,
        fill: CardColor,
        border: Option<(f32, CardColor)>,
    },
    Text {
        text: SharedString,
        bounds: CardRect,
        font_size: f32,
        line_height: f32,
        color: CardColor,
        align: TextAlign,
    },
    Image {
        asset: CanvasImageAsset,
        bounds: CardRect,
        fit: CardImageFit,
        radius: f32,
    },
}

#[derive(Default)]
struct CardScene {
    height: f32,
    primitives: Vec<CardPrimitive>,
}

#[derive(Clone, Copy)]
struct CanvasNodeFrame {
    node_index: usize,
    screen_x: f32,
    screen_y: f32,
    height: f32,
    targeted: bool,
}

#[derive(Clone)]
enum CanvasClickTarget {
    Image { node_id: String, url: String },
    TogglePrompt(String),
    Retry(String),
}

struct Toast {
    text: String,
    error: bool,
    undo: Option<(String, String)>,
    serial: u64,
}

enum DragState {
    Canvas {
        start: Point<Pixels>,
        origin: (f32, f32),
    },
    Lightbox {
        start: Point<Pixels>,
        origin: (f32, f32),
    },
    Node {
        id: String,
        start: Point<Pixels>,
        origin: Position,
        click_target: Option<CanvasClickTarget>,
    },
}

struct AppView {
    engine: GenerationEngine,
    receiver: async_channel::Receiver<RepositoryEvent>,
    board_id: Option<String>,
    board: Option<Board>,
    prompt: Entity<TextInput>,
    modal_input: Entity<TextInput>,
    search_input: Entity<TextInput>,
    focus: FocusHandle,
    lightbox_focus: FocusHandle,
    overlay: Overlay,
    target: Option<ComposerTarget>,
    attachments: Vec<PathBuf>,
    aspect_index: usize,
    count: usize,
    activity: HashMap<String, String>,
    hovered_node: Option<String>,
    expanded_prompts: HashSet<String>,
    armed_board_delete: Option<String>,
    toast: Option<Toast>,
    toast_serial: u64,
    layout: HashMap<String, Position>,
    heights: HashMap<String, f32>,
    prompt_lines: HashMap<String, Vec<SharedString>>,
    output_layouts: HashMap<String, OutputLayout>,
    canvas_nodes: Arc<Vec<CanvasNode>>,
    image_ratios: HashMap<String, f32>,
    image_assets: HashMap<String, ImageAsset>,
    transient_positions: HashMap<String, Position>,
    camera_x: f32,
    camera_y: f32,
    zoom: f32,
    drag: Option<DragState>,
}

pub fn run() -> Result<()> {
    let (sender, receiver) = async_channel::bounded(1_024);
    let repository = crate::storage::Repository::open(sender)?;
    let engine = GenerationEngine::new(repository);
    let engine_for_quit = engine.clone();
    application().run(move |cx: &mut App| {
        bind_keys(cx);
        configure_menus(cx);
        let quit_engine = engine_for_quit.clone();
        cx.on_app_quit(move |_| {
            let engine = quit_engine.clone();
            async move { engine.stop_all_for_quit() }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1500.), px(950.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(900.), px(600.))),
                titlebar: Some(TitlebarOptions {
                    title: Some(APP_NAME.into()),
                    appears_transparent: cfg!(target_os = "macos"),
                    traffic_light_position: cfg!(target_os = "macos")
                        .then(|| point(px(14.), px(14.))),
                }),
                ..Default::default()
            },
            {
                let engine = engine.clone();
                let receiver = receiver.clone();
                move |window, cx| {
                    let view = cx.new(|cx| AppView::new(engine, receiver, window, cx));
                    window.focus(&view.focus_handle(cx), cx);
                    window.on_window_should_close(cx, {
                        let weak = view.downgrade();
                        move |_, cx| {
                            weak.update(cx, |view, cx| {
                                if view.engine.active_count() > 0 {
                                    view.overlay = Overlay::QuitConfirm;
                                    cx.notify();
                                    false
                                } else {
                                    true
                                }
                            })
                            .unwrap_or(true)
                        }
                    });
                    view
                }
            },
        )
        .expect("failed to open CodexImage window");
        cx.activate(true);
    });
    Ok(())
}

fn bind_keys(cx: &mut App) {
    const CANVAS_CONTEXT: &str = "CodexImage && !CodexImageInput";

    cx.bind_keys([
        KeyBinding::new("enter", Generate, None),
        KeyBinding::new("shift-enter", InsertNewline, Some("CodexImageInput")),
        KeyBinding::new("/", FocusPrompt, Some(CANVAS_CONTEXT)),
        KeyBinding::new("cmd-k", OpenBoards, None),
        KeyBinding::new("g", ToggleGallery, Some(CANVAS_CONTEXT)),
        KeyBinding::new("f", FitCanvas, Some(CANVAS_CONTEXT)),
        KeyBinding::new("cmd-=", ZoomIn, None),
        KeyBinding::new("cmd--", ZoomOut, None),
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
        KeyBinding::new("cmd-o", AddAttachment, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("backspace", InputBackspace, Some("CodexImageInput")),
        KeyBinding::new("shift-backspace", InputBackspace, Some("CodexImageInput")),
        KeyBinding::new("ctrl-h", InputBackspace, Some("CodexImageInput")),
        KeyBinding::new("delete", InputDelete, Some("CodexImageInput")),
        KeyBinding::new("ctrl-d", InputDelete, Some("CodexImageInput")),
        KeyBinding::new("left", InputLeft, Some("CodexImageInput")),
        KeyBinding::new("right", InputRight, Some("CodexImageInput")),
        KeyBinding::new("up", InputUp, Some("CodexImageInput")),
        KeyBinding::new("down", InputDown, Some("CodexImageInput")),
        KeyBinding::new("shift-left", InputSelectLeft, Some("CodexImageInput")),
        KeyBinding::new("shift-right", InputSelectRight, Some("CodexImageInput")),
        KeyBinding::new("shift-up", InputSelectUp, Some("CodexImageInput")),
        KeyBinding::new("shift-down", InputSelectDown, Some("CodexImageInput")),
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
        KeyBinding::new("shift-home", InputSelectHome, Some("CodexImageInput")),
        KeyBinding::new("shift-end", InputSelectEnd, Some("CodexImageInput")),
        KeyBinding::new("home", InputHome, Some("CodexImageInput")),
        KeyBinding::new("end", InputEnd, Some("CodexImageInput")),
    ]);
}

fn configure_menus(cx: &mut App) {
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
        ]),
    ]);
}

impl AppView {
    fn new(
        engine: GenerationEngine,
        receiver: async_channel::Receiver<RepositoryEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prompt = cx.new(|cx| TextInput::auto_growing("Describe the image you want…", 7, cx));
        let modal_input = cx.new(|cx| TextInput::single_line("Type here…", cx));
        let search_input = cx.new(|cx| TextInput::single_line("Search boards…", cx));
        cx.subscribe(&prompt, |this, _, event, cx| {
            this.handle_input_event(event, cx)
        })
        .detach();
        cx.observe(&search_input, |_, _, cx| cx.notify()).detach();
        let summaries = engine.repository().summaries(&engine.active_node_ids());
        let board_id = summaries.first().map(|summary| summary.id.clone());
        let board = board_id
            .as_deref()
            .and_then(|id| engine.repository().board(id));
        let mut view = Self {
            engine,
            receiver,
            board_id,
            board,
            prompt,
            modal_input,
            search_input,
            focus: cx.focus_handle(),
            lightbox_focus: cx.focus_handle(),
            overlay: Overlay::None,
            target: None,
            attachments: Vec::new(),
            aspect_index: 0,
            count: 1,
            activity: HashMap::new(),
            hovered_node: None,
            expanded_prompts: HashSet::new(),
            armed_board_delete: None,
            toast: None,
            toast_serial: 0,
            layout: HashMap::new(),
            heights: HashMap::new(),
            prompt_lines: HashMap::new(),
            output_layouts: HashMap::new(),
            canvas_nodes: Arc::new(Vec::new()),
            image_ratios: HashMap::new(),
            image_assets: HashMap::new(),
            transient_positions: HashMap::new(),
            camera_x: 80.,
            camera_y: 90.,
            zoom: 1.,
            drag: None,
        };
        view.refresh_image_metadata();
        view.refresh_layout();
        let receiver = view.receiver.clone();
        cx.spawn(async move |weak, cx| {
            while let Ok(event) = receiver.recv().await {
                if weak
                    .update(cx, |view, cx| view.handle_repository_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |weak, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if weak
                    .update(cx, |view, cx| {
                        if view.engine.active_count() > 0 {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        window.focus(&view.focus, cx);
        view
    }

    fn handle_repository_event(&mut self, event: RepositoryEvent, cx: &mut Context<Self>) {
        match event {
            RepositoryEvent::Changed => {
                if let Some(id) = self.board_id.as_deref() {
                    self.board = self.engine.repository().board(id);
                }
                self.refresh_image_metadata();
                self.refresh_layout();
            }
            RepositoryEvent::Activity { node_id, text } => {
                self.activity.insert(node_id, text);
            }
        }
        cx.notify();
    }

    fn handle_input_event(&mut self, event: &TextInputEvent, cx: &mut Context<Self>) {
        match event {
            TextInputEvent::PastedImages(images) => {
                for image in images {
                    if let Ok(path) = self.save_pending_clipboard_image(image) {
                        self.attachments.push(path);
                    }
                }
            }
            TextInputEvent::PastedPaths(paths) => self.queue_attachments(paths.clone()),
        }
        cx.notify();
    }

    fn save_pending_clipboard_image(&self, image: &Image) -> Result<PathBuf> {
        let extension = match image.format {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Webp => "webp",
            ImageFormat::Gif => "gif",
            ImageFormat::Svg => "svg",
            ImageFormat::Bmp => "bmp",
            ImageFormat::Tiff => "tiff",
            ImageFormat::Ico => "ico",
            ImageFormat::Pnm => "pnm",
        };
        let directory = self
            .engine
            .repository()
            .paths()
            .root
            .join("pending-attachments");
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("clipboard-{}.{}", Uuid::new_v4(), extension));
        fs::write(&path, &image.bytes)?;
        Ok(path)
    }

    fn queue_attachments(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            if self.attachments.len() >= crate::model::MAX_ATTACHMENTS {
                break;
            }
            if path.is_file()
                && image::ImageFormat::from_path(&path).is_ok()
                && !self.attachments.contains(&path)
            {
                self.attachments.push(path);
            }
        }
    }

    fn refresh_layout(&mut self) {
        if let Some(board) = &self.board {
            let prompt_lines = board
                .nodes
                .iter()
                .map(|node| {
                    (
                        node.id.clone(),
                        wrap_prompt(&node.prompt, PROMPT_WRAP_COLUMNS)
                            .into_iter()
                            .map(SharedString::from)
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<HashMap<_, _>>();
            let output_layouts = board
                .nodes
                .iter()
                .map(|node| (node.id.clone(), output_layout(node, &self.image_ratios)))
                .collect::<HashMap<_, _>>();
            let heights = board
                .nodes
                .iter()
                .map(|node| {
                    let total_prompt_lines =
                        prompt_lines.get(&node.id).map_or(1, |lines| lines.len());
                    let output_height = output_layouts
                        .get(&node.id)
                        .map_or(0., OutputLayout::height);
                    (
                        node.id.clone(),
                        card_height_from_metadata(
                            node,
                            self.expanded_prompts.contains(&node.id),
                            total_prompt_lines,
                            output_height,
                        ),
                    )
                })
                .collect();
            let layout = compute_layout(&board.nodes, &heights);
            let canvas_nodes = board
                .nodes
                .iter()
                .map(|node| {
                    let node_prompt_lines = prompt_lines
                        .get(&node.id)
                        .cloned()
                        .unwrap_or_else(|| vec![SharedString::default()]);
                    let collapsed_prompt_lines = node_prompt_lines
                        .iter()
                        .take(COLLAPSED_PROMPT_LINES)
                        .enumerate()
                        .map(|(index, line)| {
                            if node_prompt_lines.len() > COLLAPSED_PROMPT_LINES
                                && index + 1 == COLLAPSED_PROMPT_LINES
                            {
                                SharedString::from(format!("{line}…"))
                            } else {
                                line.clone()
                            }
                        })
                        .collect();
                    let displayed_images = displayed_urls(node)
                        .iter()
                        .map(|url| CanvasImage {
                            url: url.clone(),
                            asset: self.canvas_image_asset(url),
                        })
                        .collect();
                    let attachment_images = node
                        .attachments
                        .iter()
                        .map(|url| self.canvas_image_asset(url))
                        .collect();
                    let mut canvas_node = CanvasNode {
                        node: node.clone(),
                        prompt_lines: node_prompt_lines,
                        collapsed_prompt_lines,
                        output_layout: output_layouts
                            .get(&node.id)
                            .cloned()
                            .unwrap_or(OutputLayout::None),
                        displayed_images,
                        attachment_images,
                        date: format_date(node.created_at).into(),
                        done_footer: done_footer(node).into(),
                        status_message: status_message(node).into(),
                        scene: CardScene::default(),
                        sprite_images: Vec::new(),
                        last_ready_sprite_tier: AtomicU8::new(NO_SPRITE_TIER),
                    };
                    canvas_node.scene =
                        build_card_scene(&canvas_node, self.expanded_prompts.contains(&node.id));
                    canvas_node.sprite_images = CARD_SPRITE_WIDTHS
                        .into_iter()
                        .map(|width| {
                            Arc::new(Image::from_bytes(
                                ImageFormat::Svg,
                                card_scene_svg(&canvas_node.scene, width).into_bytes(),
                            ))
                        })
                        .collect();
                    canvas_node
                })
                .collect();
            self.prompt_lines = prompt_lines;
            self.output_layouts = output_layouts;
            self.canvas_nodes = Arc::new(canvas_nodes);
            self.heights = heights;
            self.layout = layout;
        } else {
            self.layout.clear();
            self.heights.clear();
            self.prompt_lines.clear();
            self.output_layouts.clear();
            self.canvas_nodes = Arc::new(Vec::new());
        }
    }

    fn refresh_image_metadata(&mut self) {
        let (Some(board_id), Some(board)) = (self.board_id.as_deref(), self.board.as_ref()) else {
            self.image_assets.clear();
            return;
        };
        let repository = self.engine.repository();
        let mut assets = HashMap::new();
        let mut ratio_candidates = Vec::new();
        let mut seen = HashSet::new();
        for url in board.nodes.iter().flat_map(|node| {
            node.images
                .iter()
                .chain(&node.attempts)
                .chain(&node.attachments)
                .chain(&node.source_images)
        }) {
            if !seen.insert(url.clone()) {
                continue;
            }
            let Some(original) = repository.image_path(board_id, url) else {
                continue;
            };
            let thumbnail_path = repository.thumbnail_path(board_id, url);
            if thumbnail_path.as_ref().is_some_and(|path| !path.exists()) {
                let _ = create_thumbnail(&original);
            }
            let thumbnail = thumbnail_path
                .filter(|path| path.exists())
                .unwrap_or_else(|| original.clone());
            if !self.image_ratios.contains_key(url) {
                ratio_candidates.push((url.clone(), thumbnail.clone()));
            }
            assets.insert(
                url.clone(),
                ImageAsset {
                    original,
                    thumbnail,
                },
            );
        }
        self.image_ratios.retain(|url, _| seen.contains(url));
        self.image_assets = assets;
        for (url, path) in ratio_candidates {
            if let Some(ratio) = read_image_ratio(&path) {
                self.image_ratios.insert(url, ratio);
            }
        }
    }

    fn card_height(&self, node: &BoardNode) -> f32 {
        self.heights
            .get(&node.id)
            .copied()
            .unwrap_or_else(|| self.measure_card_height(node))
    }

    fn measure_card_height(&self, node: &BoardNode) -> f32 {
        card_height(
            node,
            self.expanded_prompts.contains(&node.id),
            &self.image_ratios,
        )
    }

    fn current_position(&self, id: &str) -> Option<Position> {
        self.transient_positions
            .get(id)
            .copied()
            .or_else(|| self.layout.get(id).copied())
    }

    fn board_id(&self) -> Result<&str> {
        self.board_id.as_deref().context("No board is open")
    }

    fn ensure_board(&mut self) -> Result<String> {
        if let Some(id) = &self.board_id {
            return Ok(id.clone());
        }
        let board = self.engine.repository().create_board()?;
        self.board_id = Some(board.id.clone());
        self.board = Some(board.clone());
        Ok(board.id)
    }

    fn show_error(&mut self, error: impl std::fmt::Display, cx: &mut Context<Self>) {
        self.show_toast(error.to_string(), true, None, cx);
    }

    fn show_toast(
        &mut self,
        text: String,
        error: bool,
        undo: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        self.toast_serial += 1;
        let serial = self.toast_serial;
        self.toast = Some(Toast {
            text,
            error,
            undo,
            serial,
        });
        cx.spawn(async move |weak, cx| {
            cx.background_executor().timer(Duration::from_secs(8)).await;
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

    fn open_board(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.board_id = Some(id.clone());
        self.board = self.engine.repository().board(&id);
        self.overlay = Overlay::None;
        self.target = None;
        self.expanded_prompts.clear();
        self.transient_positions.clear();
        self.camera_x = 80.;
        self.camera_y = 90.;
        self.zoom = 1.;
        self.refresh_image_metadata();
        self.refresh_layout();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn fit_canvas(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(board) = &self.board else { return };
        if board.nodes.is_empty() {
            return;
        }
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for node in &board.nodes {
            let Some(position) = self.current_position(&node.id) else {
                continue;
            };
            min_x = min_x.min(position.x);
            min_y = min_y.min(position.y);
            max_x = max_x.max(position.x + CARD_WIDTH);
            max_y = max_y.max(position.y + self.card_height(node));
        }
        let viewport = window.viewport_size();
        let width = f32::from(viewport.width) - 100.;
        let height = f32::from(viewport.height) - 150.;
        self.zoom = (width / (max_x - min_x).max(1.))
            .min(height / (max_y - min_y).max(1.))
            .clamp(0.08, 1.);
        self.camera_x =
            (f32::from(viewport.width) - (max_x - min_x) * self.zoom) / 2. - min_x * self.zoom;
        self.camera_y =
            (f32::from(viewport.height) - (max_y - min_y) * self.zoom) / 2. - min_y * self.zoom;
        cx.notify();
    }

    fn zoom_in(&mut self, _: &ZoomIn, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size();
        self.zoom_at(point(viewport.width / 2., viewport.height / 2.), 1.25, cx);
    }

    fn zoom_out(&mut self, _: &ZoomOut, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size();
        self.zoom_at(point(viewport.width / 2., viewport.height / 2.), 0.8, cx);
    }

    fn generate(&mut self, _: &Generate, window: &mut Window, cx: &mut Context<Self>) {
        match &self.overlay {
            Overlay::Lightbox(lightbox) => {
                let text = self.modal_input.read(cx).content().trim().to_owned();
                if text.is_empty() {
                    return;
                }
                let request = NewNodesRequest {
                    prompt: text,
                    parent_id: Some(lightbox.node_id.clone()),
                    source_images: Some(vec![lightbox.image.clone()]),
                    aspect: self
                        .board
                        .as_ref()
                        .and_then(|board| {
                            board.nodes.iter().find(|node| node.id == lightbox.node_id)
                        })
                        .map(|node| node.aspect.clone())
                        .unwrap_or_else(|| "auto".into()),
                    count: 1,
                    attachment_paths: vec![],
                    attachment_urls: vec![],
                };
                let result = self
                    .board_id()
                    .map(str::to_owned)
                    .and_then(|id| self.engine.add_and_start(&id, request).map(|_| id));
                match result {
                    Ok(_) => {
                        self.modal_input.update(cx, |input, cx| input.clear(cx));
                        self.overlay = Overlay::None;
                        window.focus(&self.focus, cx);
                    }
                    Err(error) => self.show_error(error, cx),
                }
                cx.notify();
                return;
            }
            Overlay::EditNode(node_id) => {
                let prompt = self.modal_input.read(cx).content().trim().to_owned();
                if prompt.is_empty() {
                    return;
                }
                let node_id = node_id.clone();
                let result = self.board_id().map(str::to_owned).and_then(|board_id| {
                    self.engine
                        .regenerate(&board_id, &node_id, Some(prompt), None)
                });
                match result {
                    Ok(()) => {
                        self.overlay = Overlay::None;
                        window.focus(&self.focus, cx);
                    }
                    Err(error) => self.show_error(error, cx),
                }
                cx.notify();
                return;
            }
            Overlay::RenameBoard(board_id) => {
                let title = self.modal_input.read(cx).content().trim().to_owned();
                let board_id = board_id.clone();
                match self.engine.repository().rename_board(&board_id, &title) {
                    Ok(()) => {
                        self.overlay = Overlay::Boards;
                        window.focus(&self.search_input.focus_handle(cx), cx);
                    }
                    Err(error) => self.show_error(error, cx),
                }
                cx.notify();
                return;
            }
            Overlay::Boards | Overlay::Gallery | Overlay::QuitConfirm => return,
            Overlay::None => {}
        }

        let text = self.prompt.read(cx).content().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let board_id = match self.ensure_board() {
            Ok(id) => id,
            Err(error) => {
                self.show_error(error, cx);
                return;
            }
        };
        let target = self.target.clone();
        let request = NewNodesRequest {
            prompt: text,
            parent_id: target.as_ref().map(|target| target.node_id.clone()),
            source_images: target.and_then(|target| target.source_image.map(|image| vec![image])),
            aspect: ASPECTS[self.aspect_index].to_owned(),
            count: self.count,
            attachment_paths: self.attachments.clone(),
            attachment_urls: Vec::new(),
        };
        match self.engine.add_and_start(&board_id, request) {
            Ok(_) => {
                for path in self.attachments.drain(..) {
                    if path.starts_with(
                        self.engine
                            .repository()
                            .paths()
                            .root
                            .join("pending-attachments"),
                    ) {
                        let _ = fs::remove_file(path);
                    }
                }
                self.prompt.update(cx, |input, cx| input.clear(cx));
                self.target = None;
                window.focus(&self.prompt.focus_handle(cx), cx);
            }
            Err(error) => self.show_error(error, cx),
        }
        cx.notify();
    }

    fn focus_prompt(&mut self, _: &FocusPrompt, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Overlay::None) {
            window.focus(&self.prompt.focus_handle(cx), cx);
        }
    }

    fn open_boards(&mut self, _: &OpenBoards, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = if matches!(self.overlay, Overlay::Boards) {
            Overlay::None
        } else {
            Overlay::Boards
        };
        self.armed_board_delete = None;
        self.search_input.update(cx, |input, cx| input.clear(cx));
        if matches!(self.overlay, Overlay::Boards) {
            window.focus(&self.search_input.focus_handle(cx), cx);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    fn toggle_gallery(&mut self, _: &ToggleGallery, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(
            self.overlay,
            Overlay::Lightbox(_)
                | Overlay::EditNode(_)
                | Overlay::RenameBoard(_)
                | Overlay::QuitConfirm
        ) {
            return;
        }
        self.overlay = if matches!(self.overlay, Overlay::Gallery) {
            Overlay::None
        } else {
            Overlay::Gallery
        };
        cx.notify();
    }

    fn fit_action(&mut self, _: &FitCanvas, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Overlay::None) {
            self.fit_canvas(window, cx)
        }
    }

    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = Overlay::None;
        self.modal_input.update(cx, |input, cx| input.clear(cx));
        window.focus(&self.focus, cx);
    }

    fn escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.overlay, Overlay::None) {
            self.close_overlay(window, cx);
        } else if self.target.is_some() {
            self.target = None;
        } else if self.prompt.focus_handle(cx).is_focused(window) {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    fn branch_hovered(&mut self, _: &BranchHovered, window: &mut Window, cx: &mut Context<Self>) {
        if let Overlay::Lightbox(lightbox) = &self.overlay {
            self.target = self.node(&lightbox.node_id).map(|node| ComposerTarget {
                node_id: node.id,
                prompt: node.prompt,
                source_image: Some(lightbox.image.clone()),
            });
            self.overlay = Overlay::None;
            window.focus(&self.prompt.focus_handle(cx), cx);
            cx.notify();
            return;
        }
        let Some(id) = self.hovered_node.clone() else {
            return;
        };
        self.branch_node(&id, None, window, cx);
    }

    fn branch_node(
        &mut self,
        id: &str,
        source_image: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(node) = self.node(id) {
            self.target = Some(ComposerTarget {
                node_id: node.id,
                prompt: node.prompt,
                source_image,
            });
            window.focus(&self.prompt.focus_handle(cx), cx);
            cx.notify();
        }
    }

    fn regenerate_hovered(
        &mut self,
        _: &RegenerateHovered,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.hovered_node.clone() else {
            return;
        };
        self.regenerate_node(&id, cx);
    }

    fn regenerate_node(&mut self, id: &str, cx: &mut Context<Self>) {
        let result = self
            .board_id()
            .map(str::to_owned)
            .and_then(|board_id| self.engine.regenerate(&board_id, id, None, None));
        if let Err(error) = result {
            self.show_error(error, cx)
        }
    }

    fn edit_hovered(&mut self, _: &EditHovered, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.hovered_node.clone() else {
            return;
        };
        self.edit_node(&id, window, cx);
    }

    fn edit_node(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node) = self.node(id) else { return };
        self.modal_input.update(cx, |input, cx| {
            input.set_mode(TextInputMode::FixedMultiline { lines: 7 }, cx);
            input.set_placeholder("Edit prompt…", cx);
            input.set_content(node.prompt, cx);
        });
        self.overlay = Overlay::EditNode(id.to_owned());
        window.focus(&self.modal_input.focus_handle(cx), cx);
        cx.notify();
    }

    fn duplicate_hovered(&mut self, _: &DuplicateHovered, _: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.hovered_node.clone() else {
            return;
        };
        self.duplicate_node(&id, cx);
    }

    fn duplicate_node(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(node) = self.node(id) else { return };
        let request = NewNodesRequest {
            prompt: node.prompt,
            parent_id: node.parent_id,
            source_images: Some(node.source_images),
            aspect: node.aspect,
            count: 1,
            attachment_paths: Vec::new(),
            attachment_urls: node.attachments,
        };
        let result = self
            .board_id()
            .map(str::to_owned)
            .and_then(|board_id| self.engine.add_and_start(&board_id, request).map(|_| ()));
        if let Err(error) = result {
            self.show_error(error, cx)
        }
    }

    fn delete_hovered(&mut self, _: &DeleteHovered, _: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.hovered_node.clone() else {
            return;
        };
        self.delete_node(&id, cx);
    }

    fn delete_node(&mut self, id: &str, cx: &mut Context<Self>) {
        let board_id = match self.board_id() {
            Ok(id) => id.to_owned(),
            Err(error) => {
                self.show_error(error, cx);
                return;
            }
        };
        match self.engine.delete_subtree(&board_id, id) {
            Ok((deleted, undo_id)) => {
                let text = if deleted.len() == 1 {
                    "Node deleted".into()
                } else {
                    format!("{} nodes deleted", deleted.len())
                };
                self.show_toast(text, false, Some((board_id, undo_id)), cx);
            }
            Err(error) => self.show_error(error, cx),
        }
    }

    fn navigate_left(&mut self, _: &LightboxLeft, window: &mut Window, cx: &mut Context<Self>) {
        if !self.lightbox_input_focused(window, cx) {
            self.navigate_lightbox(-1, 0, cx)
        }
    }
    fn navigate_right(&mut self, _: &LightboxRight, window: &mut Window, cx: &mut Context<Self>) {
        if !self.lightbox_input_focused(window, cx) {
            self.navigate_lightbox(1, 0, cx)
        }
    }
    fn navigate_up(&mut self, _: &LightboxUp, window: &mut Window, cx: &mut Context<Self>) {
        if !self.lightbox_input_focused(window, cx) {
            self.navigate_lightbox(0, -1, cx)
        }
    }
    fn navigate_down(&mut self, _: &LightboxDown, window: &mut Window, cx: &mut Context<Self>) {
        if !self.lightbox_input_focused(window, cx) {
            self.navigate_lightbox(0, 1, cx)
        }
    }

    fn lightbox_input_focused(&self, window: &Window, cx: &Context<Self>) -> bool {
        matches!(self.overlay, Overlay::Lightbox(_))
            && self.modal_input.focus_handle(cx).is_focused(window)
    }

    fn open_lightbox(
        &mut self,
        node_id: String,
        image: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal_input.update(cx, |input, cx| {
            input.set_mode(TextInputMode::SingleLine, cx);
            input.set_placeholder("Refine this image…", cx);
            input.clear(cx);
        });
        self.overlay = Overlay::Lightbox(Lightbox {
            node_id,
            image,
            zoom: LIGHTBOX_MIN_ZOOM,
            pan_x: 0.,
            pan_y: 0.,
            pending: None,
        });
        window.focus(&self.lightbox_focus, cx);
        cx.notify();
    }

    fn navigate_lightbox(&mut self, horizontal: i32, vertical: i32, cx: &mut Context<Self>) {
        let Overlay::Lightbox(current) = &self.overlay else {
            return;
        };
        let Some(board) = &self.board else { return };
        let origin = current.navigation_origin();
        let Some(target) = lightbox_target(board, &origin, horizontal, vertical) else {
            return;
        };
        let Overlay::Lightbox(current) = &mut self.overlay else {
            return;
        };
        current.request(target);
        self.modal_input.update(cx, |input, cx| input.clear(cx));
        cx.notify();
    }

    fn prepare_lightbox_assets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = match &self.overlay {
            Overlay::Lightbox(lightbox) => lightbox.pending.clone(),
            _ => return,
        };
        let mut load_error = None;
        if let Some(pending) = pending {
            let resource = Resource::Path(Arc::from(self.display_image_path(&pending.image, true)));
            match window.use_asset::<ImgResourceLoader>(&resource, cx) {
                Some(Ok(image)) if image.frame_count() > 0 => {
                    if let Overlay::Lightbox(lightbox) = &mut self.overlay {
                        lightbox.commit_pending(&pending);
                    }
                }
                Some(Ok(_)) => load_error = Some("Image contains no displayable frames".to_owned()),
                Some(Err(error)) => load_error = Some(format!("Could not load image: {error}")),
                None => {}
            }
        }
        if let Some(error) = load_error {
            if let Overlay::Lightbox(lightbox) = &mut self.overlay {
                lightbox.pending = None;
            }
            self.show_error(error, cx);
        }

        let Some((board, current)) = self.board.as_ref().and_then(|board| {
            let Overlay::Lightbox(lightbox) = &self.overlay else {
                return None;
            };
            Some((board, lightbox.displayed_location()))
        }) else {
            return;
        };
        let current_resource =
            Resource::Path(Arc::from(self.display_image_path(&current.image, true)));
        let _ = window.use_asset::<ImgResourceLoader>(&current_resource, cx);

        let mut previous_path = None;
        for horizontal in [-1, 1] {
            let Some(target) = lightbox_target(board, &current, horizontal, 0) else {
                continue;
            };
            let path = self.display_image_path(&target.image, true);
            if previous_path.as_ref() == Some(&path) {
                continue;
            }
            previous_path = Some(path.clone());
            let resource = Resource::Path(Arc::from(path));
            let _ = window.use_asset::<ImgResourceLoader>(&resource, cx);
        }
    }

    fn add_attachment(&mut self, _: &AddAttachment, _: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach images".into()),
        });
        cx.spawn(async move |weak, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let _ = weak.update(cx, |view, cx| {
                view.queue_attachments(paths);
                cx.notify();
            });
        })
        .detach();
    }

    fn quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        if self.engine.active_count() > 0 {
            self.overlay = Overlay::QuitConfirm;
            cx.notify();
        } else {
            cx.quit();
        }
    }

    fn node(&self, id: &str) -> Option<BoardNode> {
        self.board
            .as_ref()?
            .nodes
            .iter()
            .find(|node| node.id == id)
            .cloned()
    }

    fn render_canvas(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let viewport = window.viewport_size();
        let minimap = self.render_minimap(viewport, cx);
        let viewport_width = f32::from(viewport.width);
        let viewport_height = f32::from(viewport.height);
        let board = self.board.as_ref();

        let visible_nodes = board
            .into_iter()
            .flat_map(|board| board.nodes.iter().enumerate())
            .filter_map(|(node_index, node)| {
                let position = self.current_position(&node.id)?;
                let screen_x = self.camera_x + position.x * self.zoom;
                let screen_y = self.camera_y + position.y * self.zoom;
                let height = self.card_height(node) * self.zoom;
                rect_is_visible(
                    screen_x,
                    screen_y,
                    CARD_WIDTH * self.zoom,
                    height,
                    viewport_width,
                    viewport_height,
                    VIEWPORT_CULL_MARGIN,
                )
                .then_some(CanvasNodeFrame {
                    node_index,
                    screen_x,
                    screen_y,
                    height,
                    targeted: self
                        .target
                        .as_ref()
                        .is_some_and(|target| target.node_id == node.id),
                })
            })
            .collect::<Vec<_>>();

        let hovered_toolbar = self.hovered_node.as_deref().and_then(|hovered_id| {
            visible_nodes.iter().find_map(|frame| {
                let node = self.canvas_nodes.get(frame.node_index)?;
                (node.node.id == hovered_id)
                    .then(|| self.render_node_toolbar(&node.node, *frame, cx))
            })
        });
        let edge_points: Vec<_> = board
            .into_iter()
            .flat_map(|board| &board.nodes)
            .filter_map(|node| {
                let parent = node.parent_id.as_deref()?;
                let parent_position = self.current_position(parent)?;
                let node_position = self.current_position(&node.id)?;
                let parent_height = self.heights.get(parent).copied()?;
                let from = point(
                    px(self.camera_x + (parent_position.x + CARD_WIDTH / 2.) * self.zoom),
                    px(self.camera_y + (parent_position.y + parent_height) * self.zoom),
                );
                let to = point(
                    px(self.camera_x + (node_position.x + CARD_WIDTH / 2.) * self.zoom),
                    px(self.camera_y + node_position.y * self.zoom),
                );
                edge_is_visible(
                    from,
                    to,
                    viewport_width,
                    viewport_height,
                    VIEWPORT_CULL_MARGIN,
                )
                .then_some((from, to))
            })
            .collect();
        let canvas_nodes = self.canvas_nodes.clone();
        let activity = self.activity.clone();
        let zoom = self.zoom;
        let camera_x = self.camera_x;
        let camera_y = self.camera_y;
        let now = now_ms();
        let background = canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                paint_dot_grid(bounds, camera_x, camera_y, zoom, window);
                if !edge_points.is_empty() {
                    let connector_style = ConnectorStyle::for_zoom(zoom);
                    let mut edges = PathBuilder::stroke(px(connector_style.stroke_width));
                    for (from, to) in &edge_points {
                        append_dashed_connector(&mut edges, *from, *to, connector_style);
                    }
                    if let Ok(path) = edges.build() {
                        window.paint_path(path, theme::line());
                    }
                }
                for frame in &visible_nodes {
                    if let Some(node) = canvas_nodes.get(frame.node_index) {
                        paint_canvas_node(*frame, node, zoom, &activity, now, window, cx);
                    }
                }
            },
        )
        .size_full();

        let mut layer = div()
            .id("canvas")
            .absolute()
            .inset_0()
            .overflow_hidden()
            .bg(theme::background())
            .cursor(gpui::CursorStyle::OpenHand)
            .on_scroll_wheel(
                cx.listener(|this, event: &ScrollWheelEvent, _, cx| this.scroll_canvas(event, cx)),
            )
            .on_pinch(cx.listener(|this, event: &PinchEvent, _, cx| this.pinch_canvas(event, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.canvas_mouse_down(event, window, cx);
                }),
            )
            .child(background);
        if let Some(toolbar) = hovered_toolbar {
            layer = layer.child(toolbar);
        }
        if let Some(minimap) = minimap {
            layer = layer.child(minimap);
        }
        layer.into_any_element()
    }

    fn render_node_toolbar(
        &self,
        node: &BoardNode,
        frame: CanvasNodeFrame,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let scale = self.zoom;
        let branch_id = node.id.clone();
        let edit_id = node.id.clone();
        let regenerate_id = node.id.clone();
        let duplicate_id = node.id.clone();
        let delete_id = node.id.clone();
        let running_id = node.id.clone();
        let copy_prompt = node.prompt.clone();
        let toolbar = div()
            .absolute()
            .top(px(-36. * scale))
            .right(px(4. * scale))
            .flex()
            .gap(px(4. * scale))
            .occlude()
            .children((node.status == NodeStatus::Running).then(|| {
                div()
                    .id(SharedString::from(format!("stop-{}", running_id)))
                    .px(px(8. * scale))
                    .py(px(4. * scale))
                    .rounded(px(6. * scale))
                    .bg(theme::background().opacity(0.9))
                    .text_color(theme::danger())
                    .text_size(px(12. * scale))
                    .child("Stop")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.engine.stop_node(&running_id);
                    }))
                    .into_any_element()
            }))
            .children((node.status != NodeStatus::Running).then(|| {
                div()
                    .id(SharedString::from(format!("branch-{}", branch_id)))
                    .px(px(8. * scale))
                    .py(px(4. * scale))
                    .rounded(px(6. * scale))
                    .bg(theme::background().opacity(0.9))
                    .text_color(theme::ink())
                    .text_size(px(12. * scale))
                    .child("Branch")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.branch_node(&branch_id, None, window, cx);
                    }))
                    .into_any_element()
            }))
            .children((node.status != NodeStatus::Running).then(|| {
                div()
                    .id(SharedString::from(format!("edit-{}", edit_id)))
                    .px(px(8. * scale))
                    .py(px(4. * scale))
                    .rounded(px(6. * scale))
                    .bg(theme::background().opacity(0.9))
                    .text_color(theme::dim())
                    .text_size(px(12. * scale))
                    .child("Edit")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.edit_node(&edit_id, window, cx);
                    }))
                    .into_any_element()
            }))
            .children((node.status != NodeStatus::Running).then(|| {
                div()
                    .id(SharedString::from(format!("regen-{}", regenerate_id)))
                    .px(px(8. * scale))
                    .py(px(4. * scale))
                    .rounded(px(6. * scale))
                    .bg(theme::background().opacity(0.9))
                    .text_color(theme::dim())
                    .text_size(px(12. * scale))
                    .child("Retry")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.regenerate_node(&regenerate_id, cx);
                    }))
                    .into_any_element()
            }))
            .child(
                div()
                    .id(SharedString::from(format!("copy-prompt-{}", node.id)))
                    .px(px(8. * scale))
                    .py(px(4. * scale))
                    .rounded(px(6. * scale))
                    .bg(theme::background().opacity(0.9))
                    .text_color(theme::dim())
                    .text_size(px(12. * scale))
                    .child("Copy")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_prompt.clone()));
                        this.show_toast("Prompt copied".into(), false, None, cx);
                    })),
            )
            .child(
                div()
                    .id(SharedString::from(format!("dup-{}", duplicate_id)))
                    .px(px(8. * scale))
                    .py(px(4. * scale))
                    .rounded(px(6. * scale))
                    .bg(theme::background().opacity(0.9))
                    .text_color(theme::dim())
                    .text_size(px(12. * scale))
                    .child("Dup")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.duplicate_node(&duplicate_id, cx);
                    })),
            )
            .child(
                div()
                    .id(SharedString::from(format!("del-{}", delete_id)))
                    .px(px(8. * scale))
                    .py(px(4. * scale))
                    .rounded(px(6. * scale))
                    .bg(theme::background().opacity(0.9))
                    .text_color(theme::danger())
                    .text_size(px(12. * scale))
                    .child("Delete")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.delete_node(&delete_id, cx);
                    })),
            );
        div()
            .absolute()
            .left(px(frame.screen_x))
            .top(px(frame.screen_y))
            .w(px(CARD_WIDTH * scale))
            .h(px(frame.height))
            .child(toolbar)
            .into_any_element()
    }

    fn render_minimap(
        &self,
        viewport: gpui::Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        const WIDTH: f32 = 142.;
        const HEIGHT: f32 = 96.;
        const PADDING: f32 = 6.;
        const RIGHT: f32 = 18.;
        const BOTTOM: f32 = 24.;

        let board = self.board.as_ref()?;
        if board.nodes.len() < 2 {
            return None;
        }
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for node in &board.nodes {
            let position = self.current_position(&node.id)?;
            min_x = min_x.min(position.x);
            min_y = min_y.min(position.y);
            max_x = max_x.max(position.x + CARD_WIDTH);
            max_y = max_y.max(position.y + self.card_height(node));
        }
        let world_width = (max_x - min_x).max(1.);
        let world_height = (max_y - min_y).max(1.);
        let scale =
            ((WIDTH - PADDING * 2.) / world_width).min((HEIGHT - PADDING * 2.) / world_height);
        let offset_x = PADDING + (WIDTH - PADDING * 2. - world_width * scale) / 2.;
        let offset_y = PADDING + (HEIGHT - PADDING * 2. - world_height * scale) / 2.;
        let node_rects: Vec<_> = board
            .nodes
            .iter()
            .filter_map(|node| {
                let position = self.current_position(&node.id)?;
                Some((
                    offset_x + (position.x - min_x) * scale,
                    offset_y + (position.y - min_y) * scale,
                    (CARD_WIDTH * scale).max(2.),
                    (self.card_height(node) * scale).max(2.),
                    node.status,
                ))
            })
            .collect();
        let viewport_width = f32::from(viewport.width);
        let viewport_height = f32::from(viewport.height);
        let visible_world_x = -self.camera_x / self.zoom;
        let visible_world_y = -self.camera_y / self.zoom;
        let visible_rect = (
            offset_x + (visible_world_x - min_x) * scale,
            offset_y + (visible_world_y - min_y) * scale,
            viewport_width / self.zoom * scale,
            viewport_height / self.zoom * scale,
        );
        let map = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                for (x, y, width, height, status) in node_rects {
                    let color = match status {
                        NodeStatus::Running => theme::accent(),
                        NodeStatus::Error => theme::danger(),
                        _ => theme::dim(),
                    };
                    window.paint_quad(fill(
                        Bounds::new(
                            point(bounds.left() + px(x), bounds.top() + px(y)),
                            size(px(width), px(height)),
                        ),
                        color.opacity(0.78),
                    ));
                }
                window.paint_quad(fill(
                    Bounds::new(
                        point(
                            bounds.left() + px(visible_rect.0),
                            bounds.top() + px(visible_rect.1),
                        ),
                        size(px(visible_rect.2), px(visible_rect.3)),
                    ),
                    theme::accent().opacity(0.18),
                ));
            },
        )
        .size_full();
        let origin_x = viewport_width - RIGHT - WIDTH;
        let origin_y = viewport_height - BOTTOM - HEIGHT;
        Some(
            div()
                .id("minimap")
                .absolute()
                .right(px(RIGHT))
                .bottom(px(BOTTOM))
                .w(px(WIDTH))
                .h(px(HEIGHT))
                .overflow_hidden()
                .rounded_lg()
                .border_1()
                .border_color(theme::line())
                .bg(theme::raised().opacity(0.94))
                .cursor_pointer()
                .occlude()
                .child(map)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let local_x = f32::from(event.position.x) - origin_x;
                        let local_y = f32::from(event.position.y) - origin_y;
                        let world_x = min_x + (local_x - offset_x) / scale;
                        let world_y = min_y + (local_y - offset_y) / scale;
                        this.camera_x = viewport_width / 2. - world_x * this.zoom;
                        this.camera_y = viewport_height / 2. - world_y * this.zoom;
                        cx.notify();
                    }),
                )
                .into_any_element(),
        )
    }

    fn display_image_path(&self, url: &str, high_res: bool) -> PathBuf {
        if let Some(asset) = self.image_assets.get(url) {
            return if high_res {
                asset.original.clone()
            } else {
                asset.thumbnail.clone()
            };
        }
        let Some(board_id) = self.board_id.as_deref() else {
            return PathBuf::new();
        };
        self.engine
            .repository()
            .image_path(board_id, url)
            .unwrap_or_default()
    }

    fn canvas_image_asset(&self, url: &str) -> CanvasImageAsset {
        CanvasImageAsset {
            original: Arc::from(self.display_image_path(url, true)),
            thumbnail: Arc::from(self.display_image_path(url, false)),
        }
    }

    fn render_composer(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let width = 660_f32.min(f32::from(window.viewport_size().width) - 40.);
        let left = (f32::from(window.viewport_size().width) - width) / 2.;
        let mut composer = div()
            .absolute()
            .left(px(left))
            .bottom(px(24.))
            .w(px(width))
            .rounded(px(16.))
            .border_1()
            .border_color(theme::line())
            .bg(theme::raised().opacity(0.97))
            .p_3()
            .occlude();
        if let Some(target) = &self.target {
            let cancel_id = target.node_id.clone();
            composer = composer.child(
                div()
                    .mb_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(theme::dim())
                    .child(div().text_color(theme::accent()).child("Branching from"))
                    .child(target.prompt.chars().take(70).collect::<String>())
                    .child(div().flex_1())
                    .child(
                        div()
                            .id(SharedString::from(format!("cancel-target-{cancel_id}")))
                            .cursor_pointer()
                            .text_color(theme::faint())
                            .child("×")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.target = None;
                                cx.notify();
                            })),
                    ),
            );
        }
        if !self.attachments.is_empty() {
            let mut strip = div().mb_2().flex().gap_2();
            for (index, path) in self.attachments.iter().enumerate() {
                strip = strip.child(
                    div()
                        .relative()
                        .child(
                            img(path.clone())
                                .size(px(42.))
                                .object_fit(ObjectFit::Cover)
                                .rounded_md(),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("remove-attachment-{index}")))
                                .absolute()
                                .top(px(-5.))
                                .right(px(-5.))
                                .size(px(16.))
                                .rounded_full()
                                .bg(theme::background())
                                .text_center()
                                .text_xs()
                                .text_color(theme::ink())
                                .cursor_pointer()
                                .child("×")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if index < this.attachments.len() {
                                        this.attachments.remove(index);
                                    }
                                    cx.notify();
                                })),
                        ),
                );
            }
            composer = composer.child(strip);
        }
        composer = composer.child(
            div()
                .flex()
                .items_end()
                .gap_2()
                .child(
                    div()
                        .id("aspect")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::line())
                        .text_xs()
                        .text_color(theme::dim())
                        .cursor_pointer()
                        .child(ASPECTS[self.aspect_index])
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.aspect_index = (this.aspect_index + 1) % ASPECTS.len();
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id("count")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::line())
                        .text_xs()
                        .text_color(theme::dim())
                        .cursor_pointer()
                        .child(format!("×{}", self.count))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.count = this.count % 4 + 1;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id("attach")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::line())
                        .text_xs()
                        .text_color(theme::dim())
                        .cursor_pointer()
                        .child("Attach")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.add_attachment(&AddAttachment, window, cx)
                        })),
                )
                .child(div().flex_1().child(self.prompt.clone()))
                .child(
                    div()
                        .id("send")
                        .size(px(28.))
                        .rounded_lg()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme::accent_strong().opacity(0.18))
                        .text_color(theme::accent())
                        .cursor_pointer()
                        .child("↑")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.generate(&Generate, window, cx)),
                        ),
                ),
        );
        composer.into_any_element()
    }

    fn render_empty(&self, _window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let mut samples = div()
            .mt_4()
            .flex()
            .flex_wrap()
            .justify_center()
            .gap_2()
            .w(px(600.));
        for sample in SAMPLES {
            let text = (*sample).to_owned();
            samples = samples.child(
                div()
                    .id(SharedString::from(format!("sample-{}", sample.len())))
                    .rounded_full()
                    .border_1()
                    .border_color(theme::line())
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(theme::dim())
                    .cursor_pointer()
                    .child(*sample)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.prompt
                            .update(cx, |input, cx| input.set_content(text.clone(), cx));
                        window.focus(&this.prompt.focus_handle(cx), cx);
                    })),
            );
        }
        div().absolute().inset_0().pb(px(170.)).flex().flex_col().items_center().justify_center()
            .child(div().text_size(px(42.)).text_color(theme::accent()).child("❖"))
            .child(div().mt_2().text_size(px(26.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::ink()).child("What should we create?"))
            .child(div().mt_2().w(px(600.)).text_center().text_sm().text_color(theme::dim()).child("Ask for one image or a complete ordered series. Use ×N for parallel takes, then branch, continue, and regenerate on an infinite canvas."))
            .child(samples)
            .child(div().mt_5().text_xs().text_color(theme::faint()).child("/ prompt   ⌘K boards   G gallery   F fit view   Esc cancel"))
            .into_any_element()
    }

    fn canvas_node_at(&self, position: Point<Pixels>) -> Option<(usize, Position)> {
        let x = f32::from(position.x);
        let y = f32::from(position.y);
        self.canvas_nodes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, canvas_node)| {
                let world_position = self.current_position(&canvas_node.node.id)?;
                let screen_x = self.camera_x + world_position.x * self.zoom;
                let screen_y = self.camera_y + world_position.y * self.zoom;
                let width = CARD_WIDTH * self.zoom;
                let height = self.card_height(&canvas_node.node) * self.zoom;
                let toolbar_top = screen_y - NODE_TOOLBAR_HEIGHT * self.zoom;
                (x >= screen_x
                    && x <= screen_x + width
                    && y >= toolbar_top
                    && y <= screen_y + height)
                    .then_some((index, world_position))
            })
    }

    fn canvas_click_target(
        &self,
        canvas_node: &CanvasNode,
        position: Point<Pixels>,
        world_position: Position,
    ) -> Option<CanvasClickTarget> {
        let local_x = (f32::from(position.x) - self.camera_x) / self.zoom - world_position.x;
        let local_y = (f32::from(position.y) - self.camera_y) / self.zoom - world_position.y;
        let expanded = self.expanded_prompts.contains(&canvas_node.node.id);
        let prompt_clamped = canvas_node.prompt_lines.len() > COLLAPSED_PROMPT_LINES;
        let visible_lines = if expanded {
            canvas_node.prompt_lines.len().min(EXPANDED_PROMPT_LINES)
        } else {
            canvas_node
                .collapsed_prompt_lines
                .len()
                .min(COLLAPSED_PROMPT_LINES)
        }
        .max(1);
        let prompt_height = 24.
            + visible_lines as f32 * PROMPT_LINE_HEIGHT
            + if prompt_clamped { SHOW_MORE_HEIGHT } else { 0. };
        if prompt_clamped
            && local_y >= 13. + visible_lines as f32 * PROMPT_LINE_HEIGHT
            && local_y <= prompt_height
        {
            return Some(CanvasClickTarget::TogglePrompt(canvas_node.node.id.clone()));
        }

        let attachment_height = if canvas_node.attachment_images.is_empty() {
            0.
        } else {
            ATTACHMENT_ROW_HEIGHT
        };
        let media_top = prompt_height + attachment_height + 26. + MEDIA_GAP;
        let media_y = local_y - media_top;
        if media_y >= 0. && media_y <= canvas_node.output_layout.height() {
            let image_index = match &canvas_node.output_layout {
                OutputLayout::None => None,
                OutputLayout::Tiles { cells, .. } => cells.iter().find_map(|cell| {
                    (local_x >= cell.x
                        && local_x <= cell.x + cell.width
                        && media_y >= cell.y
                        && media_y <= cell.y + cell.height)
                        .then_some(cell.index)
                }),
                OutputLayout::Filmstrip {
                    hero_height,
                    compact_count,
                    strip_cell_width,
                    ..
                } => {
                    if media_y <= *hero_height {
                        Some(0)
                    } else {
                        let strip_y = media_y - *hero_height - MEDIA_GAP;
                        if strip_y < 0. || strip_y > *strip_cell_width {
                            None
                        } else {
                            (0..*compact_count).find_map(|compact_index| {
                                let x = compact_index as f32 * (*strip_cell_width + MEDIA_GAP);
                                (local_x >= x && local_x <= x + *strip_cell_width)
                                    .then_some(compact_index + 1)
                            })
                        }
                    }
                }
            };
            if let Some(image) =
                image_index.and_then(|index| canvas_node.displayed_images.get(index))
            {
                return Some(CanvasClickTarget::Image {
                    node_id: canvas_node.node.id.clone(),
                    url: image.url.clone(),
                });
            }
        }

        let status_y = media_top + canvas_node.output_layout.height();
        let in_retry = match canvas_node.node.status {
            NodeStatus::Error if canvas_node.displayed_images.is_empty() => {
                (CARD_WIDTH / 2. - 29. ..=CARD_WIDTH / 2. + 29.).contains(&local_x)
                    && (status_y + 88. ..=status_y + 114.).contains(&local_y)
            }
            NodeStatus::Error | NodeStatus::Stopped => {
                (CARD_WIDTH - 68. ..=CARD_WIDTH - 14.).contains(&local_x)
                    && (status_y..=status_y + status_area_height(&canvas_node.node))
                        .contains(&local_y)
            }
            NodeStatus::Running | NodeStatus::Done => false,
        };
        in_retry.then(|| CanvasClickTarget::Retry(canvas_node.node.id.clone()))
    }

    fn canvas_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.overlay, Overlay::None) {
            return;
        }
        window.focus(&self.focus, cx);
        if let Some((index, origin)) = self.canvas_node_at(event.position) {
            let canvas_node = &self.canvas_nodes[index];
            self.drag = Some(DragState::Node {
                id: canvas_node.node.id.clone(),
                start: event.position,
                origin,
                click_target: self.canvas_click_target(canvas_node, event.position, origin),
            });
        } else {
            self.drag = Some(DragState::Canvas {
                start: event.position,
                origin: (self.camera_x, self.camera_y),
            });
        }
        cx.notify();
    }

    fn scroll_canvas(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        if !matches!(self.overlay, Overlay::None) {
            return;
        }
        let delta = event.delta.pixel_delta(px(18.));
        if event.modifiers.control || event.modifiers.platform {
            let factor = (-f32::from(delta.y) * 0.004).exp();
            self.zoom_at(event.position, factor, cx);
        } else {
            self.camera_x += f32::from(delta.x);
            self.camera_y += f32::from(delta.y);
            cx.notify();
        }
    }

    fn pinch_canvas(&mut self, event: &PinchEvent, cx: &mut Context<Self>) {
        if matches!(self.overlay, Overlay::None) {
            self.zoom_at(event.position, 1. + event.delta, cx)
        }
    }

    fn zoom_at(&mut self, position: Point<Pixels>, factor: f32, cx: &mut Context<Self>) {
        let old = self.zoom;
        let new = (old * factor).clamp(0.08, 2.);
        let world_x = (f32::from(position.x) - self.camera_x) / old;
        let world_y = (f32::from(position.y) - self.camera_y) / old;
        self.camera_x = f32::from(position.x) - world_x * new;
        self.camera_y = f32::from(position.y) - world_y * new;
        self.zoom = new;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.overlay, Overlay::None) {
            return;
        }
        match &self.drag {
            Some(DragState::Canvas { start, origin }) if event.dragging() => {
                self.camera_x = origin.0 + f32::from(event.position.x - start.x);
                self.camera_y = origin.1 + f32::from(event.position.y - start.y);
                cx.notify();
            }
            Some(DragState::Node {
                id, start, origin, ..
            }) if event.dragging()
                && (f32::from(event.position.x - start.x).powi(2)
                    + f32::from(event.position.y - start.y).powi(2))
                    >= 9. =>
            {
                self.transient_positions.insert(
                    id.clone(),
                    Position {
                        x: origin.x + f32::from(event.position.x - start.x) / self.zoom,
                        y: origin.y + f32::from(event.position.y - start.y) / self.zoom,
                    },
                );
                cx.notify();
            }
            _ if !event.dragging() => {
                let hovered = self
                    .canvas_node_at(event.position)
                    .and_then(|(index, _)| self.canvas_nodes.get(index))
                    .map(|node| node.node.id.clone());
                if hovered != self.hovered_node {
                    self.hovered_node = hovered;
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    fn lightbox_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(DragState::Lightbox { start, origin }) = &self.drag else {
            return;
        };
        if !event.dragging() {
            return;
        }
        let start = *start;
        let origin = *origin;
        let image_ratio = match &self.overlay {
            Overlay::Lightbox(lightbox) => self
                .image_ratios
                .get(&lightbox.image)
                .copied()
                .unwrap_or(1.),
            _ => return,
        };
        let viewport = window.viewport_size();
        if let Overlay::Lightbox(lightbox) = &mut self.overlay {
            lightbox.pan_to(
                origin.0 + f32::from(event.position.x - start.x),
                origin.1 + f32::from(event.position.y - start.y),
                f32::from(viewport.width),
                f32::from(viewport.height),
                image_ratio,
            );
            cx.notify();
        }
    }

    fn lightbox_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(&self.drag, Some(DragState::Lightbox { .. })) {
            self.drag = None;
            cx.notify();
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.overlay, Overlay::None) {
            self.drag = None;
            return;
        }
        let drag = self.drag.take();
        if let Some(DragState::Node {
            id, click_target, ..
        }) = drag
        {
            if let Some(position) = self.transient_positions.remove(&id) {
                let result = self.board_id().map(str::to_owned).and_then(|board_id| {
                    self.engine
                        .repository()
                        .move_node(&board_id, &id, position.x, position.y)
                });
                if let Err(error) = result {
                    self.show_error(error, cx);
                }
            } else if let Some(click_target) = click_target {
                match click_target {
                    CanvasClickTarget::Image { node_id, url } => {
                        self.open_lightbox(node_id, url, window, cx);
                    }
                    CanvasClickTarget::TogglePrompt(node_id) => {
                        if !self.expanded_prompts.remove(&node_id) {
                            self.expanded_prompts.insert(node_id);
                        }
                        self.refresh_layout();
                    }
                    CanvasClickTarget::Retry(node_id) => self.regenerate_node(&node_id, cx),
                }
            }
        }
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let title = self
            .board
            .as_ref()
            .map(|board| board.title.as_str())
            .unwrap_or(APP_NAME);
        let generating = self.engine.active_count() > 0;
        let mut header = div()
            .id("board-switcher")
            .absolute()
            .top(px(14.))
            .left(px(18.))
            .flex()
            .items_center()
            .gap_2()
            .rounded_xl()
            .border_1()
            .border_color(theme::line())
            .bg(theme::raised().opacity(0.96))
            .px_3()
            .py_2()
            .cursor_pointer()
            .occlude()
            .on_click(cx.listener(|this, _, window, cx| this.open_boards(&OpenBoards, window, cx)))
            .child(div().text_color(theme::accent()).child("❖"))
            .child(
                div()
                    .max_w(px(230.))
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::ink())
                    .child(title.to_owned()),
            );
        if generating {
            header = header.child(div().size(px(8.)).rounded_full().bg(theme::accent()));
        }
        header
            .child(div().text_xs().text_color(theme::faint()).child("⌄"))
            .into_any_element()
    }

    fn render_gallery_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.board
            .as_ref()?
            .nodes
            .iter()
            .any(|node| !node.images.is_empty())
            .then(|| {
                div()
                    .id("gallery-button")
                    .absolute()
                    .top(px(14.))
                    .right(px(18.))
                    .rounded_xl()
                    .border_1()
                    .border_color(theme::line())
                    .bg(theme::raised().opacity(0.96))
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(theme::dim())
                    .cursor_pointer()
                    .occlude()
                    .child("▦  Gallery")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_gallery(&ToggleGallery, window, cx)
                    }))
                    .into_any_element()
            })
    }

    fn render_boards(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let query = self.search_input.read(cx).content().to_lowercase();
        let summaries = self
            .engine
            .repository()
            .summaries(&self.engine.active_node_ids());
        let width = 360.;
        let mut list = div()
            .id("board-list")
            .max_h(px(
                (f32::from(window.viewport_size().height) * 0.58).max(300.)
            ))
            .overflow_y_scroll()
            .px_2()
            .pb_2();
        for summary in summaries
            .into_iter()
            .filter(|summary| summary.title.to_lowercase().contains(&query))
        {
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
            if let Some(url) = &summary.last_image {
                thumbnail = img(self.display_image_path(url, false))
                    .size(px(34.))
                    .rounded_md()
                    .object_fit(ObjectFit::Cover)
                    .into_any_element();
            }
            list = list.child(
                div()
                    .id(SharedString::from(format!("board-{}", summary.id)))
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
                    .on_click(cx.listener(move |this, _, window, cx| this.open_board(id.clone(), window, cx)))
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
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .text_color(theme::faint())
                            .hover(|style| style.bg(theme::hover()).text_color(theme::ink()))
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
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .text_color(theme::danger())
                            .child(if armed { "Sure?" } else { "Delete" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if this.armed_board_delete.as_deref() == Some(&delete_id) {
                                    match this.engine.delete_board(&delete_id) {
                                        Ok(()) => {
                                            let next = this
                                                .engine
                                                .repository()
                                                .summaries(&this.engine.active_node_ids())
                                                .first()
                                                .map(|summary| summary.id.clone());
                                            this.board_id = next.clone();
                                            this.board = next
                                                .as_deref()
                                                .and_then(|id| this.engine.repository().board(id));
                                            this.armed_board_delete = None;
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
                    .border_t_1()
                    .border_color(theme::line())
                    .px_4()
                    .py_3()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::accent())
                    .cursor_pointer()
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

    fn render_gallery(&self, cx: &mut Context<Self>) -> AnyElement {
        let board = self.board.clone();
        let image_count: usize = board
            .as_ref()
            .map(|board| board.nodes.iter().map(|node| node.images.len()).sum())
            .unwrap_or(0);
        let node_count = board.as_ref().map(|board| board.nodes.len()).unwrap_or(0);
        let mut content = div()
            .id("gallery-scroll")
            .flex_1()
            .overflow_y_scroll()
            .px_6()
            .pb_8();
        if let Some(board) = board {
            let depths = node_depths(&board);
            let mut nodes = board.nodes.clone();
            nodes.sort_by_key(|node| (std::cmp::Reverse(node.created_at), node.id.clone()));
            for node in nodes {
                let depth = depths.get(&node.id).copied().unwrap_or(0);
                let locate_id = node.id.clone();
                let mut strip = div().flex_1().flex().flex_wrap().gap_2();
                if node.images.is_empty() {
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
                            .child(status_label(&node)),
                    );
                } else {
                    for (index, url) in node.images.iter().enumerate() {
                        let node_id = node.id.clone();
                        let image_url = url.clone();
                        strip = strip.child(
                            div()
                                .relative()
                                .child(
                                    img(self.display_image_path(url, false))
                                        .id(SharedString::from(format!(
                                            "gallery-image-{}-{index}",
                                            node.id
                                        )))
                                        .role(Role::Button)
                                        .aria_label(format!(
                                            "Open image {} of {}",
                                            index + 1,
                                            node.images.len()
                                        ))
                                        .size(px(148.))
                                        .rounded_lg()
                                        .object_fit(ObjectFit::Cover)
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_lightbox(
                                                node_id.clone(),
                                                image_url.clone(),
                                                window,
                                                cx,
                                            );
                                        })),
                                )
                                .when(node.images.len() > 1, |cell| {
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
                                            .child(format!("{}/{}", index + 1, node.images.len())),
                                    )
                                }),
                        );
                    }
                }
                content = content.child(
                    div()
                        .border_b_1()
                        .border_color(theme::line().opacity(0.7))
                        .py_4()
                        .flex()
                        .gap_5()
                        .child(
                            div()
                                .w(px(330.))
                                .pl(px(depth as f32 * 18.))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme::ink())
                                        .child(node.prompt.clone()),
                                )
                                .child(div().mt_1().text_xs().text_color(theme::faint()).child(
                                    format!(
                                        "{} · {} · {} branch depth",
                                        status_label(&node),
                                        format_date(node.created_at),
                                        depth
                                    ),
                                ))
                                .child(
                                    div()
                                        .id(SharedString::from(format!("locate-{}", locate_id)))
                                        .mt_2()
                                        .text_xs()
                                        .text_color(theme::accent())
                                        .cursor_pointer()
                                        .child("◎ Show on canvas")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.locate_node(&locate_id, window, cx);
                                        })),
                                ),
                        )
                        .child(strip),
                );
            }
        }
        div()
            .id("gallery")
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
                            .size(px(32.))
                            .rounded_lg()
                            .border_1()
                            .border_color(theme::line())
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme::dim())
                            .cursor_pointer()
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

    fn locate_node(&mut self, node_id: &str, window: &Window, cx: &mut Context<Self>) {
        let Some(position) = self.current_position(node_id) else {
            return;
        };
        self.overlay = Overlay::None;
        self.zoom = 1.;
        self.camera_x =
            f32::from(window.viewport_size().width) / 2. - (position.x + CARD_WIDTH / 2.);
        self.camera_y = 100. - position.y;
        cx.notify();
    }

    fn render_lightbox(
        &self,
        lightbox: &Lightbox,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = self.display_image_path(&lightbox.image, true);
        let thumbnail_path = self.display_image_path(&lightbox.image, false);
        let resource = Resource::Path(Arc::from(path.clone()));
        let display_image = match window.use_asset::<ImgResourceLoader>(&resource, cx) {
            Some(Ok(image)) if image.frame_count() > 0 => img(image),
            _ => img(thumbnail_path),
        };
        let node = self.node(&lightbox.node_id);
        let image_index = node
            .as_ref()
            .and_then(|node| {
                node.images
                    .iter()
                    .position(|image| image == &lightbox.image)
            })
            .unwrap_or(0);
        let total = node.as_ref().map(|node| node.images.len()).unwrap_or(1);
        let copy_path = path.clone();
        let save_path = path.clone();
        let open_path = path.clone();
        let branch_node = lightbox.node_id.clone();
        let branch_image = lightbox.image.clone();
        let locate_node = lightbox.node_id.clone();
        let viewport_width = f32::from(window.viewport_size().width);
        let viewport_height = f32::from(window.viewport_size().height);
        let image_ratio = normalized_image_ratio(
            self.image_ratios
                .get(&lightbox.image)
                .copied()
                .unwrap_or(1.),
        );
        let (fit_width, fit_height) =
            fitted_image_size(viewport_width, viewport_height, image_ratio);
        let image_width = fit_width * lightbox.zoom;
        let image_height = fit_height * lightbox.zoom;
        let (pan_x, pan_y) = lightbox.clamped_pan(viewport_width, viewport_height, image_ratio);
        let image_left = (viewport_width - image_width) / 2. + pan_x;
        let image_top = (viewport_height - image_height) / 2. + pan_y;
        let lightbox_dragging = matches!(&self.drag, Some(DragState::Lightbox { .. }));
        let stage_cursor = if lightbox_dragging {
            gpui::CursorStyle::ClosedHand
        } else if lightbox.zoom > LIGHTBOX_MIN_ZOOM {
            gpui::CursorStyle::OpenHand
        } else {
            gpui::CursorStyle::Arrow
        };
        let continue_width = (viewport_width - 48.).clamp(240., 540.);
        let toolbar = div()
            .absolute()
            .top(px(16.))
            .right(px(16.))
            .flex()
            .gap_2()
            .occlude()
            .child(control_button(
                "Branch (B)",
                cx.listener(move |this, _, window, cx| {
                    this.branch_node(&branch_node, Some(branch_image.clone()), window, cx);
                    this.overlay = Overlay::None;
                }),
            ))
            .child(control_button(
                "Show on canvas",
                cx.listener(move |this, _, window, cx| this.locate_node(&locate_node, window, cx)),
            ))
            .child(control_button(
                "Copy",
                cx.listener(move |this, _, _, cx| this.copy_image(&copy_path, cx)),
            ))
            .child(control_button(
                "Save…",
                cx.listener(move |this, _, _, cx| this.save_image(save_path.clone(), cx)),
            ))
            .child(control_button(
                "Open original",
                cx.listener(move |_, _, _, _| {
                    let _ = std::process::Command::new("open").arg(&open_path).spawn();
                }),
            ))
            .child(control_button(
                "×",
                cx.listener(|this, _, window, cx| {
                    this.close_overlay(window, cx);
                    cx.notify();
                }),
            ));

        let mut root = div()
            .id("lightbox")
            .role(Role::Dialog)
            .aria_label("Image lightbox")
            .absolute()
            .inset_0()
            .key_context("CodexImage CodexImageLightbox")
            .track_focus(&self.lightbox_focus)
            .overflow_hidden()
            .bg(gpui::black().opacity(0.97))
            .occlude()
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                let image_ratio = match &this.overlay {
                    Overlay::Lightbox(lightbox) => this
                        .image_ratios
                        .get(&lightbox.image)
                        .copied()
                        .unwrap_or(1.),
                    _ => return,
                };
                let viewport = window.viewport_size();
                if let Overlay::Lightbox(lightbox) = &mut this.overlay {
                    let delta = event.delta.pixel_delta(px(18.));
                    lightbox.zoom_at(
                        (-f32::from(delta.y) * 0.004).exp(),
                        event.position,
                        f32::from(viewport.width),
                        f32::from(viewport.height),
                        image_ratio,
                    );
                    cx.notify();
                }
            }))
            .on_pinch(cx.listener(|this, event: &PinchEvent, window, cx| {
                let image_ratio = match &this.overlay {
                    Overlay::Lightbox(lightbox) => this
                        .image_ratios
                        .get(&lightbox.image)
                        .copied()
                        .unwrap_or(1.),
                    _ => return,
                };
                let viewport = window.viewport_size();
                if let Overlay::Lightbox(lightbox) = &mut this.overlay {
                    lightbox.zoom_at(
                        1. + event.delta,
                        event.position,
                        f32::from(viewport.width),
                        f32::from(viewport.height),
                        image_ratio,
                    );
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(Self::lightbox_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::lightbox_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::lightbox_mouse_up))
            .child(
                div()
                    .id("lightbox-stage")
                    .absolute()
                    .inset_0()
                    .overflow_hidden()
                    .cursor(stage_cursor)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            window.focus(&this.lightbox_focus, cx);
                            if let Overlay::Lightbox(lightbox) = &this.overlay
                                && lightbox.zoom > LIGHTBOX_MIN_ZOOM
                            {
                                this.drag = Some(DragState::Lightbox {
                                    start: event.position,
                                    origin: (lightbox.pan_x, lightbox.pan_y),
                                });
                                cx.notify();
                            }
                        }),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        if matches!(&this.overlay, Overlay::Lightbox(lightbox) if lightbox.zoom <= 1.)
                        {
                            this.close_overlay(window, cx);
                            cx.notify();
                        }
                    }))
                    .child(
                        display_image
                            .absolute()
                            .left(px(image_left))
                            .top(px(image_top))
                            .w(px(image_width))
                            .h(px(image_height))
                            .object_fit(ObjectFit::Contain),
                    ),
            )
            .child(toolbar)
            .child(
                div()
                    .absolute()
                    .left(px((viewport_width - continue_width) / 2.))
                    .bottom(px(20.))
                    .w(px(continue_width))
                    .rounded_xl()
                    .border_1()
                    .border_color(theme::line())
                    .bg(theme::raised().opacity(0.94))
                    .occlude()
                    .p_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .px_3()
                            .child(self.modal_input.clone()),
                    )
                    .child(
                        div()
                            .id("quick-continue")
                            .rounded_lg()
                            .bg(theme::accent_strong())
                            .px_4()
                            .py_2()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(gpui::white())
                            .cursor_pointer()
                            .child("Continue ↵")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.generate(&Generate, window, cx)
                            })),
                    )
            );
        if total > 1 {
            root = root.child(
                div()
                    .id("lightbox-position")
                    .absolute()
                    .top(px(16.))
                    .left(px(16.))
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::line())
                    .bg(theme::raised().opacity(0.9))
                    .px_3()
                    .py_2()
                    .role(Role::Label)
                    .aria_label(format!("Image {} of {}", image_index + 1, total))
                    .text_sm()
                    .text_color(theme::ink())
                    .child(format!("{} / {}", image_index + 1, total)),
            );
        }
        if lightbox.zoom > 1. {
            root = root.child(
                div()
                    .absolute()
                    .left(px(20.))
                    .bottom(px(20.))
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::line())
                    .bg(theme::raised().opacity(0.9))
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(theme::dim())
                    .child(format!("{}%", (lightbox.zoom * 100.).round() as i32)),
            );
        }
        root.into_any_element()
    }

    fn copy_image(&mut self, path: &Path, cx: &mut Context<Self>) {
        match fs::read(path).and_then(|bytes| {
            let format = image_format_for_path(path)
                .ok_or_else(|| std::io::Error::other("unsupported image format"))?;
            cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(format, bytes)));
            Ok(())
        }) {
            Ok(()) => self.show_toast("Image copied".into(), false, None, cx),
            Err(error) => self.show_error(error, cx),
        }
    }

    fn save_image(&mut self, source: PathBuf, cx: &mut Context<Self>) {
        let suggested = source
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "image.png".into());
        let directory = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| self.engine.repository().paths().root.clone());
        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested));
        cx.spawn(async move |weak, cx| {
            let Ok(Ok(Some(destination))) = receiver.await else {
                return;
            };
            let result = smol::unblock(move || fs::copy(source, destination)).await;
            let _ = weak.update(cx, |view, cx| match result {
                Ok(_) => view.show_toast("Image saved".into(), false, None, cx),
                Err(error) => view.show_error(error, cx),
            });
        })
        .detach();
    }

    fn render_modal(
        &self,
        title: &str,
        detail: &str,
        action: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("modal-overlay")
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

    fn render_quit_confirm(&self, cx: &mut Context<Self>) -> AnyElement {
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
                    .w(px(480.))
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

    fn render_toast(&self, toast: &Toast, window: &Window, cx: &mut Context<Self>) -> AnyElement {
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
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::accent_strong())
                    .px_3()
                    .py_1()
                    .text_color(theme::accent())
                    .cursor_pointer()
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
                .text_color(theme::faint())
                .cursor_pointer()
                .child("×")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toast = None;
                    cx.notify();
                })),
        )
        .into_any_element()
    }
}

fn lightbox_target(
    board: &Board,
    current: &LightboxLocation,
    horizontal: i32,
    vertical: i32,
) -> Option<LightboxLocation> {
    if horizontal == 0 && vertical == 0 {
        return None;
    }
    let node = board.nodes.iter().find(|node| node.id == current.node_id)?;
    if horizontal != 0 {
        let index = node
            .images
            .iter()
            .position(|image| image == &current.image)
            .unwrap_or(0) as i32
            + horizontal;
        if index >= 0 && (index as usize) < node.images.len() {
            return Some(LightboxLocation {
                node_id: node.id.clone(),
                image: node.images[index as usize].clone(),
            });
        }
        let parent_id = node.parent_id.as_ref()?;
        let mut siblings: Vec<_> = board
            .nodes
            .iter()
            .filter(|candidate| {
                candidate.parent_id.as_ref() == Some(parent_id) && !candidate.images.is_empty()
            })
            .collect();
        siblings.sort_by_key(|candidate| (candidate.created_at, &candidate.id));
        let current_index = siblings
            .iter()
            .position(|candidate| candidate.id == node.id)?;
        let sibling_index = current_index as i32 + horizontal;
        if sibling_index < 0 || (sibling_index as usize) >= siblings.len() {
            return None;
        }
        let sibling = siblings[sibling_index as usize];
        let image = if horizontal > 0 {
            sibling.images.first()
        } else {
            sibling.images.last()
        }?;
        return Some(LightboxLocation {
            node_id: sibling.id.clone(),
            image: image.clone(),
        });
    }
    if vertical < 0 {
        let mut parent_id = node.parent_id.as_deref();
        while let Some(id) = parent_id {
            let parent = board.nodes.iter().find(|candidate| candidate.id == id)?;
            if let Some(image) = parent.images.last() {
                return Some(LightboxLocation {
                    node_id: parent.id.clone(),
                    image: image.clone(),
                });
            }
            parent_id = parent.parent_id.as_deref();
        }
        return None;
    }

    let mut queue: VecDeque<&BoardNode> = board
        .nodes
        .iter()
        .filter(|candidate| candidate.parent_id.as_deref() == Some(&node.id))
        .collect();
    while let Some(child) = queue.pop_front() {
        if let Some(image) = child.images.last() {
            return Some(LightboxLocation {
                node_id: child.id.clone(),
                image: image.clone(),
            });
        }
        queue.extend(
            board
                .nodes
                .iter()
                .filter(|candidate| candidate.parent_id.as_deref() == Some(&child.id)),
        );
    }
    None
}

impl Focusable for AppView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prepare_lightbox_assets(window, cx);
        let empty = self
            .board
            .as_ref()
            .is_none_or(|board| board.nodes.is_empty());
        let mut root = div()
            .key_context("CodexImage")
            .track_focus(&self.focus)
            .size_full()
            .overflow_hidden()
            .bg(theme::background())
            .on_action(cx.listener(Self::generate))
            .on_action(cx.listener(Self::focus_prompt))
            .on_action(cx.listener(Self::open_boards))
            .on_action(cx.listener(Self::toggle_gallery))
            .on_action(cx.listener(Self::fit_action))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::escape))
            .on_action(cx.listener(Self::branch_hovered))
            .on_action(cx.listener(Self::regenerate_hovered))
            .on_action(cx.listener(Self::edit_hovered))
            .on_action(cx.listener(Self::duplicate_hovered))
            .on_action(cx.listener(Self::delete_hovered))
            .on_action(cx.listener(Self::navigate_left))
            .on_action(cx.listener(Self::navigate_right))
            .on_action(cx.listener(Self::navigate_up))
            .on_action(cx.listener(Self::navigate_down))
            .on_action(cx.listener(Self::add_attachment))
            .on_action(cx.listener(Self::quit))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .can_drop(|value, _, _| value.downcast_ref::<ExternalPaths>().is_some())
            .drag_over::<ExternalPaths>(|style, _, _, _| {
                style.border_2().border_color(theme::accent())
            })
            .on_drop(cx.listener(|this, dropped: &ExternalPaths, _, cx| {
                this.queue_attachments(dropped.paths().to_vec());
                cx.notify();
            }));

        if !empty {
            root = root.child(self.render_canvas(window, cx));
        } else {
            root = root.child(self.render_empty(window, cx));
        }
        root = root.child(self.render_header(cx));
        if let Some(button) = self.render_gallery_button(cx) {
            root = root.child(button);
        }
        root = root.child(self.render_composer(window, cx));
        root = match &self.overlay {
            Overlay::None => root,
            Overlay::Boards => root.child(self.render_boards(window, cx)),
            Overlay::Gallery => root.child(self.render_gallery(cx)),
            Overlay::Lightbox(lightbox) => root.child(self.render_lightbox(lightbox, window, cx)),
            Overlay::EditNode(_) => root.child(self.render_modal(
                "Edit prompt",
                "Update the request and regenerate this node in a fresh Codex session.",
                "Save & regenerate",
                cx,
            )),
            Overlay::RenameBoard(_) => root.child(self.render_modal(
                "Rename board",
                "Choose a concise name for this exploration.",
                "Rename",
                cx,
            )),
            Overlay::QuitConfirm => root.child(self.render_quit_confirm(cx)),
        };
        if let Some(toast) = &self.toast {
            root = root.child(self.render_toast(toast, window, cx));
        }
        root
    }
}

fn control_button(
    label: impl Into<SharedString>,
    listener: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let label = label.into();
    div()
        .id(label.clone())
        .role(Role::Button)
        .aria_label(label.clone())
        .rounded_lg()
        .border_1()
        .border_color(theme::line())
        .bg(theme::raised())
        .px_3()
        .py_2()
        .text_sm()
        .text_color(theme::dim())
        .cursor_pointer()
        .hover(|style| style.border_color(theme::faint()).text_color(theme::ink()))
        .child(label)
        .on_click(listener)
        .into_any_element()
}

const PROMPT_LINE_HEIGHT: f32 = 18.;
const HEADER_FIXED_HEIGHT: f32 = 50.;
const SHOW_MORE_HEIGHT: f32 = 18.;
const ATTACHMENT_ROW_HEIGHT: f32 = 44.;
const PROMPT_WRAP_COLUMNS: usize = 42;
const COLLAPSED_PROMPT_LINES: usize = 6;
const EXPANDED_PROMPT_LINES: usize = 18;
const MEDIA_GAP: f32 = 1.;
const GRID_GAP: f32 = 28.;
const GRID_DOT_SIZE: f32 = 1.4;
const GRID_TILE_CELLS: usize = 32;
const GRID_TILE_SIZE: f32 = GRID_GAP * GRID_TILE_CELLS as f32;
const GRID_TEXTURE_SCALE: u32 = 2;
const GRID_ANTIALIAS_SAMPLES: u32 = 8;
const GRID_COLOR_BGRA: [u8; 3] = [0x2d, 0x22, 0x1e];
const VIEWPORT_CULL_MARGIN: f32 = 96.;
const CONNECTOR_STROKE_WIDTH: f32 = 1.6;
const CONNECTOR_DASH_LENGTH: f32 = 7.;
const CONNECTOR_GAP_LENGTH: f32 = 5.;
const CARD_SPRITE_WIDTHS: [f32; 4] = [85., 170., 340., 680.];
const NO_SPRITE_TIER: u8 = u8::MAX;

impl CardScene {
    fn quad(
        &mut self,
        bounds: CardRect,
        radius: f32,
        fill: CardColor,
        border: Option<(f32, CardColor)>,
    ) {
        self.primitives.push(CardPrimitive::Quad {
            bounds,
            radius,
            fill,
            border,
        });
    }

    fn text(
        &mut self,
        text: impl Into<SharedString>,
        bounds: CardRect,
        font_size: f32,
        line_height: f32,
        color: CardColor,
        align: TextAlign,
    ) {
        self.primitives.push(CardPrimitive::Text {
            text: text.into(),
            bounds,
            font_size,
            line_height,
            color,
            align,
        });
    }

    fn image(&mut self, asset: CanvasImageAsset, bounds: CardRect, fit: CardImageFit, radius: f32) {
        self.primitives.push(CardPrimitive::Image {
            asset,
            bounds,
            fit,
            radius,
        });
    }
}

fn build_card_scene(canvas_node: &CanvasNode, expanded: bool) -> CardScene {
    let node = &canvas_node.node;
    let mut scene = CardScene {
        height: card_height_from_metadata(
            node,
            expanded,
            canvas_node.prompt_lines.len(),
            canvas_node.output_layout.height(),
        ),
        primitives: Vec::new(),
    };
    let border = if node.status == NodeStatus::Running {
        CardColor::Accent45
    } else {
        CardColor::Line
    };
    scene.quad(
        CardRect::new(0., 0., CARD_WIDTH, scene.height),
        20.,
        CardColor::Raised,
        Some((1., border)),
    );

    let prompt_clamped = canvas_node.prompt_lines.len() > COLLAPSED_PROMPT_LINES;
    let visible_prompt_lines = if expanded {
        &canvas_node.prompt_lines[..canvas_node.prompt_lines.len().min(EXPANDED_PROMPT_LINES)]
    } else {
        &canvas_node.collapsed_prompt_lines
    };
    let visible_line_count = visible_prompt_lines.len().max(1);
    let prompt_block_height = 24.
        + visible_line_count as f32 * PROMPT_LINE_HEIGHT
        + if prompt_clamped { SHOW_MORE_HEIGHT } else { 0. };
    for (index, line) in visible_prompt_lines.iter().enumerate() {
        scene.text(
            line.clone(),
            CardRect::new(
                14.,
                13. + index as f32 * PROMPT_LINE_HEIGHT,
                CARD_WIDTH - 28.,
                PROMPT_LINE_HEIGHT,
            ),
            12.5,
            PROMPT_LINE_HEIGHT,
            CardColor::Ink90,
            TextAlign::Left,
        );
    }
    if prompt_clamped {
        scene.text(
            if expanded { "Show less" } else { "Show more" },
            CardRect::new(
                14.,
                13. + visible_line_count as f32 * PROMPT_LINE_HEIGHT,
                CARD_WIDTH - 28.,
                SHOW_MORE_HEIGHT,
            ),
            10.5,
            SHOW_MORE_HEIGHT,
            CardColor::Accent,
            TextAlign::Left,
        );
    }

    let mut cursor_y = prompt_block_height;
    if !canvas_node.attachment_images.is_empty() {
        for (index, asset) in canvas_node.attachment_images.iter().enumerate() {
            scene.image(
                asset.clone(),
                CardRect::new(14. + index as f32 * 42., cursor_y + 8., 36., 36.),
                CardImageFit::Cover,
                6.,
            );
        }
        cursor_y += ATTACHMENT_ROW_HEIGHT;
    }

    scene.text(
        "❖ Me",
        CardRect::new(14., cursor_y, 42., 18.),
        10.5,
        18.,
        CardColor::Faint,
        TextAlign::Left,
    );
    if node.aspect != "auto" {
        let pill_width = (node.aspect.len() as f32 * 6.2 + 8.).max(28.);
        let pill = CardRect::new(56., cursor_y + 1., pill_width, 16.);
        scene.quad(
            pill,
            8.,
            CardColor::Transparent,
            Some((1., CardColor::Line)),
        );
        scene.text(
            node.aspect.clone(),
            pill,
            10.5,
            16.,
            CardColor::Faint,
            TextAlign::Center,
        );
    }
    scene.text(
        canvas_node.date.clone(),
        CardRect::new(140., cursor_y, CARD_WIDTH - 154., 18.),
        10.5,
        18.,
        CardColor::Faint,
        TextAlign::Right,
    );
    cursor_y += 26.;
    scene.quad(
        CardRect::new(0., cursor_y, CARD_WIDTH, MEDIA_GAP),
        0.,
        CardColor::Line,
        None,
    );
    cursor_y += MEDIA_GAP;

    match &canvas_node.output_layout {
        OutputLayout::None => {}
        OutputLayout::Tiles { height, cells } => {
            let media = CardRect::new(0., cursor_y, CARD_WIDTH, *height);
            scene.quad(media, 0., CardColor::Hover, None);
            if cells.is_empty() {
                scene.quad(media, 0., CardColor::Raised, None);
                scene.text(
                    "Generating…",
                    media,
                    12.,
                    *height,
                    CardColor::Faint,
                    TextAlign::Center,
                );
            }
            for cell in cells {
                let Some(image) = canvas_node.displayed_images.get(cell.index) else {
                    continue;
                };
                let bounds = CardRect::new(cell.x, cursor_y + cell.y, cell.width, cell.height);
                scene.quad(bounds, 0., CardColor::Raised, None);
                scene.image(image.asset.clone(), bounds, CardImageFit::Contain, 0.);
            }
            if node.images.is_empty() && !canvas_node.displayed_images.is_empty() {
                let badge = CardRect::new(CARD_WIDTH - 76., cursor_y + *height - 27., 68., 19.);
                scene.quad(badge, 5., CardColor::Background82, None);
                scene.text(
                    "Unfinalized",
                    badge,
                    10.,
                    19.,
                    CardColor::Dim,
                    TextAlign::Center,
                );
            }
            cursor_y += *height;
        }
        OutputLayout::Filmstrip {
            height,
            hero_height,
            compact_count,
            hidden_count,
            strip_cell_width,
        } => {
            scene.quad(
                CardRect::new(0., cursor_y, CARD_WIDTH, *height),
                0.,
                CardColor::Line,
                None,
            );
            if let Some(hero) = canvas_node.displayed_images.first() {
                let hero_bounds = CardRect::new(0., cursor_y, CARD_WIDTH, *hero_height);
                scene.quad(hero_bounds, 0., CardColor::Raised, None);
                scene.image(hero.asset.clone(), hero_bounds, CardImageFit::Contain, 0.);
            }
            let badge = CardRect::new(CARD_WIDTH - 57., cursor_y + *hero_height - 25., 49., 19.);
            scene.quad(badge, 5., CardColor::Background82, None);
            scene.text(
                format!("1 / {}", canvas_node.displayed_images.len()),
                badge,
                10.,
                19.,
                CardColor::Ink,
                TextAlign::Center,
            );
            let strip_y = cursor_y + *hero_height + MEDIA_GAP;
            for compact_index in 0..*compact_count {
                let Some(image) = canvas_node.displayed_images.get(compact_index + 1) else {
                    continue;
                };
                let bounds = CardRect::new(
                    compact_index as f32 * (*strip_cell_width + MEDIA_GAP),
                    strip_y,
                    *strip_cell_width,
                    *strip_cell_width,
                );
                scene.quad(bounds, 0., CardColor::Raised, None);
                scene.image(image.asset.clone(), bounds, CardImageFit::Cover, 0.);
            }
            if *hidden_count > 0 {
                let hidden = CardRect::new(
                    CARD_WIDTH - *strip_cell_width,
                    strip_y,
                    *strip_cell_width,
                    *strip_cell_width,
                );
                scene.quad(hidden, 0., CardColor::Raised, None);
                scene.text(
                    format!("+{hidden_count}"),
                    hidden,
                    12.,
                    *strip_cell_width,
                    CardColor::Dim,
                    TextAlign::Center,
                );
            }
            cursor_y += *height;
        }
    }

    let status_height = status_area_height(node);
    match node.status {
        NodeStatus::Running => {}
        NodeStatus::Done => scene.text(
            canvas_node.done_footer.clone(),
            CardRect::new(14., cursor_y, CARD_WIDTH - 28., status_height),
            10.5,
            status_height,
            CardColor::Dim,
            TextAlign::Left,
        ),
        NodeStatus::Error if canvas_node.displayed_images.is_empty() => {
            scene.text(
                "!",
                CardRect::new(0., cursor_y + 17., CARD_WIDTH, 24.),
                18.,
                24.,
                CardColor::Danger,
                TextAlign::Center,
            );
            scene.text(
                canvas_node.status_message.clone(),
                CardRect::new(18., cursor_y + 45., CARD_WIDTH - 36., 32.),
                11.5,
                16.,
                CardColor::Danger,
                TextAlign::Center,
            );
            let retry = CardRect::new(CARD_WIDTH / 2. - 29., cursor_y + 88., 58., 26.);
            scene.quad(
                retry,
                7.,
                CardColor::Transparent,
                Some((1., CardColor::Line)),
            );
            scene.text("Retry", retry, 10.5, 26., CardColor::Dim, TextAlign::Center);
        }
        NodeStatus::Error | NodeStatus::Stopped => {
            scene.text(
                canvas_node.status_message.clone(),
                CardRect::new(14., cursor_y, CARD_WIDTH - 90., status_height),
                10.8,
                status_height,
                if node.status == NodeStatus::Error {
                    CardColor::Danger
                } else {
                    CardColor::Faint
                },
                TextAlign::Left,
            );
            let retry = CardRect::new(
                CARD_WIDTH - 68.,
                cursor_y + (status_height - 26.) * 0.5,
                54.,
                26.,
            );
            scene.quad(
                retry,
                7.,
                CardColor::Transparent,
                Some((1., CardColor::Line)),
            );
            scene.text("Retry", retry, 10.5, 26., CardColor::Dim, TextAlign::Center);
        }
    }
    scene
}

fn card_scene_svg(scene: &CardScene, rendered_width: f32) -> String {
    let rendered_height = scene.height * rendered_width / CARD_WIDTH;
    let mut svg = String::with_capacity(scene.primitives.len() * 180);
    write!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{rendered_width}\" height=\"{rendered_height}\" viewBox=\"0 0 {CARD_WIDTH} {}\">",
        scene.height
    )
    .expect("writing to a String cannot fail");
    write!(
        svg,
        "<defs><clipPath id=\"card\"><rect x=\"0\" y=\"0\" width=\"{CARD_WIDTH}\" height=\"{}\" rx=\"20\"/></clipPath></defs><g clip-path=\"url(#card)\">",
        scene.height
    )
    .expect("writing to a String cannot fail");
    for (index, primitive) in scene.primitives.iter().enumerate() {
        match primitive {
            CardPrimitive::Quad {
                bounds,
                radius,
                fill,
                border,
            } => {
                let (fill_color, fill_opacity) = fill.svg();
                write!(
                    svg,
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{radius}\" fill=\"{fill_color}\" fill-opacity=\"{fill_opacity}\"",
                    bounds.x, bounds.y, bounds.width, bounds.height
                )
                .expect("writing to a String cannot fail");
                if let Some((width, color)) = border {
                    let (stroke, opacity) = color.svg();
                    write!(
                        svg,
                        " stroke=\"{stroke}\" stroke-opacity=\"{opacity}\" stroke-width=\"{width}\""
                    )
                    .expect("writing to a String cannot fail");
                }
                svg.push_str("/>");
            }
            CardPrimitive::Text {
                text,
                bounds,
                font_size,
                line_height,
                color,
                align,
            } => {
                let clip_id = format!("text-{index}");
                write!(
                    svg,
                    "<clipPath id=\"{clip_id}\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/></clipPath>",
                    bounds.x, bounds.y, bounds.width, bounds.height
                )
                .expect("writing to a String cannot fail");
                let (anchor, x) = match align {
                    TextAlign::Left => ("start", bounds.x),
                    TextAlign::Center => ("middle", bounds.x + bounds.width / 2.),
                    TextAlign::Right => ("end", bounds.x + bounds.width),
                };
                let baseline = bounds.y + (line_height - font_size) * 0.5 + font_size * 0.82;
                let (fill, opacity) = color.svg();
                write!(
                    svg,
                    "<text x=\"{x}\" y=\"{baseline}\" clip-path=\"url(#{clip_id})\" font-family=\"system-ui,sans-serif\" font-size=\"{font_size}\" font-weight=\"400\" text-anchor=\"{anchor}\" fill=\"{fill}\" fill-opacity=\"{opacity}\">"
                )
                .expect("writing to a String cannot fail");
                push_xml_escaped(&mut svg, text);
                svg.push_str("</text>");
            }
            CardPrimitive::Image {
                asset,
                bounds,
                fit,
                radius,
            } => {
                if asset.thumbnail.as_os_str().is_empty() {
                    continue;
                }
                let clip_id = format!("image-{index}");
                write!(
                    svg,
                    "<clipPath id=\"{clip_id}\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"{radius}\"/></clipPath><image x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" clip-path=\"url(#{clip_id})\" preserveAspectRatio=\"xMidYMid {}\" href=\"",
                    bounds.x,
                    bounds.y,
                    bounds.width,
                    bounds.height,
                    bounds.x,
                    bounds.y,
                    bounds.width,
                    bounds.height,
                    match fit {
                        CardImageFit::Contain => "meet",
                        CardImageFit::Cover => "slice",
                    }
                )
                .expect("writing to a String cannot fail");
                push_xml_escaped(&mut svg, &asset.thumbnail.to_string_lossy());
                svg.push_str("\"/>");
            }
        }
    }
    svg.push_str("</g></svg>");
    svg
}

fn push_xml_escaped(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            _ => output.push(character),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DotGridMetrics {
    tile_size: f32,
    origin_x: f32,
    origin_y: f32,
}

fn dot_grid_metrics(camera_x: f32, camera_y: f32, zoom: f32) -> DotGridMetrics {
    let zoom = zoom.max(0.0001);
    let tile_size = GRID_TILE_SIZE * zoom;
    let dot_offset = GRID_GAP * zoom / 2.;
    DotGridMetrics {
        tile_size,
        origin_x: (camera_x - dot_offset).rem_euclid(tile_size) - tile_size,
        origin_y: (camera_y - dot_offset).rem_euclid(tile_size) - tile_size,
    }
}

fn dot_grid_texture_pixels() -> image::RgbaImage {
    let texture_size = (GRID_TILE_SIZE * GRID_TEXTURE_SCALE as f32).round() as u32;
    let mut texture = image::RgbaImage::new(texture_size, texture_size);
    let scale = GRID_TEXTURE_SCALE as f32;
    let dot_radius = GRID_DOT_SIZE * scale / 2.;
    let samples_per_pixel = GRID_ANTIALIAS_SAMPLES.pow(2);

    for row in 0..GRID_TILE_CELLS {
        let center_y = (GRID_GAP / 2. + row as f32 * GRID_GAP) * scale;
        for column in 0..GRID_TILE_CELLS {
            let center_x = (GRID_GAP / 2. + column as f32 * GRID_GAP) * scale;
            let min_x = (center_x - dot_radius).floor().max(0.) as u32;
            let max_x = (center_x + dot_radius).ceil().min(texture_size as f32) as u32;
            let min_y = (center_y - dot_radius).floor().max(0.) as u32;
            let max_y = (center_y + dot_radius).ceil().min(texture_size as f32) as u32;

            for pixel_y in min_y..max_y {
                for pixel_x in min_x..max_x {
                    let mut covered_samples = 0;
                    for sample_y in 0..GRID_ANTIALIAS_SAMPLES {
                        let sample_y = pixel_y as f32
                            + (sample_y as f32 + 0.5) / GRID_ANTIALIAS_SAMPLES as f32;
                        for sample_x in 0..GRID_ANTIALIAS_SAMPLES {
                            let sample_x = pixel_x as f32
                                + (sample_x as f32 + 0.5) / GRID_ANTIALIAS_SAMPLES as f32;
                            let dx = sample_x - center_x;
                            let dy = sample_y - center_y;
                            covered_samples +=
                                u32::from(dx * dx + dy * dy <= dot_radius * dot_radius);
                        }
                    }
                    if covered_samples > 0 {
                        let alpha = ((covered_samples * 255 + samples_per_pixel / 2)
                            / samples_per_pixel) as u8;
                        texture.put_pixel(
                            pixel_x,
                            pixel_y,
                            image::Rgba([
                                GRID_COLOR_BGRA[0],
                                GRID_COLOR_BGRA[1],
                                GRID_COLOR_BGRA[2],
                                alpha,
                            ]),
                        );
                    }
                }
            }
        }
    }
    texture
}

fn dot_grid_image() -> Arc<gpui::RenderImage> {
    static IMAGE: OnceLock<Arc<gpui::RenderImage>> = OnceLock::new();
    IMAGE
        .get_or_init(|| {
            Arc::new(gpui::RenderImage::new(vec![image::Frame::new(
                dot_grid_texture_pixels(),
            )]))
        })
        .clone()
}

fn paint_dot_grid(
    bounds: Bounds<Pixels>,
    camera_x: f32,
    camera_y: f32,
    zoom: f32,
    window: &mut Window,
) {
    let image = dot_grid_image();
    let metrics = dot_grid_metrics(camera_x, camera_y, zoom);
    let tile_size = px(metrics.tile_size);
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let mut y = bounds.top() + px(metrics.origin_y);
        while y < bounds.bottom() {
            let mut x = bounds.left() + px(metrics.origin_x);
            while x < bounds.right() {
                let _ = window.paint_image(
                    Bounds {
                        origin: point(x, y),
                        size: size(tile_size, tile_size),
                    },
                    px(0.).into(),
                    image.clone(),
                    0,
                    false,
                );
                x += tile_size;
            }
            y += tile_size;
        }
    });
}

fn rect_is_visible(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    viewport_width: f32,
    viewport_height: f32,
    margin: f32,
) -> bool {
    x + width >= -margin
        && y + height >= -margin
        && x <= viewport_width + margin
        && y <= viewport_height + margin
}

fn edge_is_visible(
    from: Point<Pixels>,
    to: Point<Pixels>,
    viewport_width: f32,
    viewport_height: f32,
    margin: f32,
) -> bool {
    let from_x = f32::from(from.x);
    let from_y = f32::from(from.y);
    let to_x = f32::from(to.x);
    let to_y = f32::from(to.y);
    rect_is_visible(
        from_x.min(to_x),
        from_y.min(to_y),
        (from_x - to_x).abs(),
        (from_y - to_y).abs(),
        viewport_width,
        viewport_height,
        margin,
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum DashCommand {
    MoveTo(Point<Pixels>),
    LineTo(Point<Pixels>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ConnectorStyle {
    stroke_width: f32,
    dash_length: f32,
    gap_length: f32,
}

impl ConnectorStyle {
    fn for_zoom(zoom: f32) -> Self {
        debug_assert!(zoom > 0.);
        Self {
            stroke_width: CONNECTOR_STROKE_WIDTH * zoom,
            dash_length: CONNECTOR_DASH_LENGTH * zoom,
            gap_length: CONNECTOR_GAP_LENGTH * zoom,
        }
    }
}

fn append_dashed_connector(
    path: &mut PathBuilder,
    from: Point<Pixels>,
    to: Point<Pixels>,
    style: ConnectorStyle,
) {
    let middle_y = px((f32::from(from.y) + f32::from(to.y)) / 2.);
    let points = [from, point(from.x, middle_y), point(to.x, middle_y), to];
    trace_dashed_polyline(
        &points,
        style.dash_length,
        style.gap_length,
        |command| match command {
            DashCommand::MoveTo(point) => path.move_to(point),
            DashCommand::LineTo(point) => path.line_to(point),
        },
    );
}

fn trace_dashed_polyline(
    points: &[Point<Pixels>],
    dash_length: f32,
    gap_length: f32,
    mut emit: impl FnMut(DashCommand),
) {
    debug_assert!(dash_length > 0. && gap_length > 0.);
    let mut drawing = true;
    let mut remaining = dash_length;
    let mut dash_open = false;

    for segment in points.windows(2) {
        let start_x = f32::from(segment[0].x);
        let start_y = f32::from(segment[0].y);
        let delta_x = f32::from(segment[1].x) - start_x;
        let delta_y = f32::from(segment[1].y) - start_y;
        let length = delta_x.hypot(delta_y);
        if length <= f32::EPSILON {
            continue;
        }
        let direction_x = delta_x / length;
        let direction_y = delta_y / length;
        let mut traveled = 0.;

        while traveled < length {
            let step = remaining.min(length - traveled);
            let fragment_start = point(
                px(start_x + direction_x * traveled),
                px(start_y + direction_y * traveled),
            );
            traveled += step;
            let fragment_end = point(
                px(start_x + direction_x * traveled),
                px(start_y + direction_y * traveled),
            );

            if drawing {
                if !dash_open {
                    emit(DashCommand::MoveTo(fragment_start));
                    dash_open = true;
                }
                emit(DashCommand::LineTo(fragment_end));
            }

            remaining -= step;
            if remaining <= f32::EPSILON {
                if drawing {
                    dash_open = false;
                }
                drawing = !drawing;
                remaining = if drawing { dash_length } else { gap_length };
            }
        }
    }
}

fn canvas_bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
}

#[derive(Clone, Copy)]
struct CanvasTextStyle {
    font_size: f32,
    line_height: f32,
    color: gpui::Hsla,
    align: TextAlign,
}

impl CanvasTextStyle {
    fn new(font_size: f32, line_height: f32, color: gpui::Hsla, align: TextAlign) -> Self {
        Self {
            font_size,
            line_height,
            color,
            align,
        }
    }
}

fn paint_canvas_text(
    text: SharedString,
    bounds: Bounds<Pixels>,
    style: CanvasTextStyle,
    window: &mut Window,
    cx: &mut App,
) {
    if text.is_empty() || style.font_size <= 0. {
        return;
    }
    let run = TextRun {
        len: text.len(),
        color: style.color,
        ..Default::default()
    };
    let line = window
        .text_system()
        .shape_line(text, px(style.font_size), &[run], None);
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let _ = line.paint(
            bounds.origin,
            px(style.line_height),
            style.align,
            Some(bounds.size.width),
            window,
            cx,
        );
    });
}

fn paint_canvas_image(
    path: &Arc<Path>,
    bounds: Bounds<Pixels>,
    fit: ObjectFit,
    corner_radius: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let resource = Resource::Path(path.clone());
    let Some(Ok(data)) = window.use_asset::<ImgResourceLoader>(&resource, cx) else {
        return;
    };
    if data.frame_count() == 0 {
        return;
    }
    let image_bounds = fit.get_bounds(bounds, data.size(0));
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        let _ = window.paint_image(image_bounds, px(corner_radius).into(), data, 0, false);
    });
}

fn paint_canvas_node(
    frame: CanvasNodeFrame,
    canvas_node: &CanvasNode,
    zoom: f32,
    activity: &HashMap<String, String>,
    now: i64,
    window: &mut Window,
    cx: &mut App,
) {
    let bounds = canvas_bounds(
        frame.screen_x,
        frame.screen_y,
        CARD_WIDTH * zoom,
        frame.height,
    );
    let tier = if zoom <= 0.25 {
        0
    } else if zoom <= 0.5 {
        1
    } else if zoom <= 1. {
        2
    } else {
        3
    };
    let mut sprite = canvas_node
        .sprite_images
        .get(tier)
        .and_then(|image| image.clone().use_render_image(window, cx));
    if sprite.is_some() {
        canvas_node
            .last_ready_sprite_tier
            .store(tier as u8, Ordering::Relaxed);
    } else {
        let previous_tier = canvas_node.last_ready_sprite_tier.load(Ordering::Relaxed) as usize;
        if previous_tier < canvas_node.sprite_images.len() && previous_tier != tier {
            sprite = canvas_node.sprite_images[previous_tier]
                .clone()
                .use_render_image(window, cx);
        }
    }
    if let Some(sprite) = sprite {
        let _ = window.paint_image(bounds, px(20. * zoom).into(), sprite, 0, false);
    } else {
        paint_card_scene(frame, &canvas_node.scene, zoom, window, cx);
    }

    paint_high_resolution_card_images(frame, &canvas_node.scene, zoom, window, cx);

    if canvas_node.node.status == NodeStatus::Running {
        let activity = activity
            .get(&canvas_node.node.id)
            .map(String::as_str)
            .unwrap_or("Working");
        paint_canvas_text(
            format!(
                "Generating · {}s · {activity}",
                (now - canvas_node
                    .node
                    .run_started_at
                    .unwrap_or(canvas_node.node.created_at))
                .max(0)
                    / 1_000
            )
            .into(),
            canvas_bounds(
                frame.screen_x + 14. * zoom,
                frame.screen_y + (canvas_node.scene.height - 42.) * zoom,
                (CARD_WIDTH - 28.) * zoom,
                42. * zoom,
            ),
            CanvasTextStyle::new(10.8 * zoom, 42. * zoom, theme::dim(), TextAlign::Left),
            window,
            cx,
        );
    }

    if frame.targeted {
        window.paint_quad(quad(
            bounds,
            px(20. * zoom),
            gpui::transparent_black(),
            px(zoom),
            theme::accent(),
            BorderStyle::Solid,
        ));
    }
}

fn paint_card_scene(
    frame: CanvasNodeFrame,
    scene: &CardScene,
    zoom: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let card_bounds = canvas_bounds(
        frame.screen_x,
        frame.screen_y,
        CARD_WIDTH * zoom,
        frame.height,
    );
    window.with_content_mask(
        Some(ContentMask {
            bounds: card_bounds,
        }),
        |window| {
            for primitive in &scene.primitives {
                match primitive {
                    CardPrimitive::Quad {
                        bounds,
                        radius,
                        fill,
                        border,
                    } => {
                        let bounds = transform_card_rect(*bounds, frame, zoom);
                        let (border_width, border_color) = border
                            .map(|(width, color)| (width * zoom, color.hsla()))
                            .unwrap_or((0., gpui::transparent_black()));
                        window.paint_quad(quad(
                            bounds,
                            px(radius * zoom),
                            fill.hsla(),
                            px(border_width),
                            border_color,
                            BorderStyle::Solid,
                        ));
                    }
                    CardPrimitive::Text {
                        text,
                        bounds,
                        font_size,
                        line_height,
                        color,
                        align,
                    } => paint_canvas_text(
                        text.clone(),
                        transform_card_rect(*bounds, frame, zoom),
                        CanvasTextStyle::new(
                            font_size * zoom,
                            line_height * zoom,
                            color.hsla(),
                            *align,
                        ),
                        window,
                        cx,
                    ),
                    CardPrimitive::Image {
                        asset,
                        bounds,
                        fit,
                        radius,
                    } => paint_canvas_image(
                        &asset.thumbnail,
                        transform_card_rect(*bounds, frame, zoom),
                        match fit {
                            CardImageFit::Contain => ObjectFit::Contain,
                            CardImageFit::Cover => ObjectFit::Cover,
                        },
                        radius * zoom,
                        window,
                        cx,
                    ),
                }
            }
        },
    );
}

fn paint_high_resolution_card_images(
    frame: CanvasNodeFrame,
    scene: &CardScene,
    zoom: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let scale_factor = window.scale_factor();
    for primitive in &scene.primitives {
        let CardPrimitive::Image {
            asset,
            bounds,
            fit,
            radius,
        } = primitive
        else {
            continue;
        };
        if asset.original.as_ref() == asset.thumbnail.as_ref()
            || !image_needs_high_resolution(*bounds, zoom, scale_factor)
        {
            continue;
        }
        paint_canvas_image(
            &asset.original,
            transform_card_rect(*bounds, frame, zoom),
            match fit {
                CardImageFit::Contain => ObjectFit::Contain,
                CardImageFit::Cover => ObjectFit::Cover,
            },
            radius * zoom,
            window,
            cx,
        );
    }
}

fn image_needs_high_resolution(bounds: CardRect, zoom: f32, scale_factor: f32) -> bool {
    bounds.width.max(bounds.height) * zoom * scale_factor > THUMBNAIL_MAX_DIMENSION as f32
}

fn transform_card_rect(bounds: CardRect, frame: CanvasNodeFrame, zoom: f32) -> Bounds<Pixels> {
    canvas_bounds(
        frame.screen_x + bounds.x * zoom,
        frame.screen_y + bounds.y * zoom,
        bounds.width * zoom,
        bounds.height * zoom,
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OutputCell {
    index: usize,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[derive(Clone, Debug, PartialEq)]
enum OutputLayout {
    None,
    Tiles {
        height: f32,
        cells: Vec<OutputCell>,
    },
    Filmstrip {
        height: f32,
        hero_height: f32,
        compact_count: usize,
        hidden_count: usize,
        strip_cell_width: f32,
    },
}

impl OutputLayout {
    fn height(&self) -> f32 {
        match self {
            Self::None => 0.,
            Self::Tiles { height, .. } | Self::Filmstrip { height, .. } => *height,
        }
    }
}

fn card_height(node: &BoardNode, expanded: bool, ratios: &HashMap<String, f32>) -> f32 {
    card_height_from_metadata(
        node,
        expanded,
        wrap_prompt(&node.prompt, PROMPT_WRAP_COLUMNS).len(),
        output_layout(node, ratios).height(),
    )
}

fn card_height_from_metadata(
    node: &BoardNode,
    expanded: bool,
    total_prompt_lines: usize,
    output_height: f32,
) -> f32 {
    let (prompt_lines, prompt_clamped) =
        prompt_metrics_from_line_count(total_prompt_lines, expanded);
    HEADER_FIXED_HEIGHT
        + prompt_lines * PROMPT_LINE_HEIGHT
        + if prompt_clamped { SHOW_MORE_HEIGHT } else { 0. }
        + if node.attachments.is_empty() {
            0.
        } else {
            ATTACHMENT_ROW_HEIGHT
        }
        + MEDIA_GAP
        + output_height
        + status_area_height(node)
}

fn prompt_metrics_from_line_count(total_lines: usize, expanded: bool) -> (f32, bool) {
    let line_limit = if expanded {
        EXPANDED_PROMPT_LINES
    } else {
        COLLAPSED_PROMPT_LINES
    };
    (
        total_lines.min(line_limit).max(1) as f32,
        total_lines > COLLAPSED_PROMPT_LINES,
    )
}

fn wrap_prompt(value: &str, max_graphemes: usize) -> Vec<String> {
    assert!(max_graphemes > 0, "prompt wrap width must be non-zero");
    let mut lines = Vec::new();

    for paragraph in value.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();
        let mut current_len = 0;
        for word in paragraph.split_whitespace() {
            let graphemes = UnicodeSegmentation::graphemes(word, true).collect::<Vec<_>>();
            if graphemes.len() <= max_graphemes {
                let separator_len = usize::from(!current.is_empty());
                if current_len + separator_len + graphemes.len() <= max_graphemes {
                    if separator_len == 1 {
                        current.push(' ');
                    }
                    current.push_str(word);
                    current_len += separator_len + graphemes.len();
                } else {
                    lines.push(std::mem::take(&mut current));
                    current.push_str(word);
                    current_len = graphemes.len();
                }
                continue;
            }

            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
            }
            for chunk in graphemes.chunks(max_graphemes) {
                let chunk = chunk.concat();
                if chunk.graphemes(true).count() == max_graphemes {
                    lines.push(chunk);
                } else {
                    current_len = chunk.graphemes(true).count();
                    current = chunk;
                }
            }
        }

        if !current.is_empty() {
            lines.push(current);
        } else if paragraph.chars().all(char::is_whitespace) {
            lines.push(String::new());
        }
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn displayed_urls(node: &BoardNode) -> &[String] {
    if node.images.is_empty() {
        node.attempts
            .last()
            .map(std::slice::from_ref)
            .unwrap_or_default()
    } else {
        &node.images
    }
}

fn output_layout(node: &BoardNode, ratios: &HashMap<String, f32>) -> OutputLayout {
    let urls = displayed_urls(node);
    match urls.len() {
        0 if node.status == NodeStatus::Running => OutputLayout::Tiles {
            height: CARD_WIDTH,
            cells: Vec::new(),
        },
        0 => OutputLayout::None,
        1 => {
            let height = image_height(CARD_WIDTH, output_ratio(node, &urls[0], ratios));
            OutputLayout::Tiles {
                height,
                cells: vec![OutputCell {
                    index: 0,
                    x: 0.,
                    y: 0.,
                    width: CARD_WIDTH,
                    height,
                }],
            }
        }
        2..=4 => {
            let cell_width = (CARD_WIDTH - MEDIA_GAP) / 2.;
            let mut cells = Vec::with_capacity(urls.len());
            let mut y = 0.;
            for (row, chunk) in urls.chunks(2).enumerate() {
                let row_height = chunk
                    .iter()
                    .map(|url| image_height(cell_width, output_ratio(node, url, ratios)))
                    .fold(0_f32, f32::max);
                for (column, _) in chunk.iter().enumerate() {
                    cells.push(OutputCell {
                        index: row * 2 + column,
                        x: column as f32 * (cell_width + MEDIA_GAP),
                        y,
                        width: cell_width,
                        height: row_height,
                    });
                }
                y += row_height;
                if row * 2 + chunk.len() < urls.len() {
                    y += MEDIA_GAP;
                }
            }
            OutputLayout::Tiles { height: y, cells }
        }
        _ => {
            let hero_height = image_height(CARD_WIDTH, output_ratio(node, &urls[0], ratios));
            let compact_count = if urls.len() > 6 {
                4
            } else {
                (urls.len() - 1).min(5)
            };
            let hidden_count = urls.len() - compact_count - 1;
            let strip_cells = compact_count + usize::from(hidden_count > 0);
            let strip_cell_width = (CARD_WIDTH - MEDIA_GAP * strip_cells.saturating_sub(1) as f32)
                / strip_cells as f32;
            OutputLayout::Filmstrip {
                height: hero_height + MEDIA_GAP + strip_cell_width,
                hero_height,
                compact_count,
                hidden_count,
                strip_cell_width,
            }
        }
    }
}

fn status_area_height(node: &BoardNode) -> f32 {
    let has_images = !displayed_urls(node).is_empty();
    match node.status {
        NodeStatus::Running | NodeStatus::Done => 42.,
        NodeStatus::Error if has_images => 64.,
        NodeStatus::Error => 132.,
        NodeStatus::Stopped => 52.,
    }
}

fn output_ratio(node: &BoardNode, url: &str, ratios: &HashMap<String, f32>) -> f32 {
    ratios
        .get(url)
        .copied()
        .or_else(|| parse_aspect_ratio(&node.aspect))
        .unwrap_or(1.)
        .clamp(0.2, 5.)
}

fn parse_aspect_ratio(aspect: &str) -> Option<f32> {
    let (width, height) = aspect.split_once(':')?;
    let (Ok(width), Ok(height)) = (width.parse::<f32>(), height.parse::<f32>()) else {
        return None;
    };
    (width.is_finite() && height.is_finite() && width > 0. && height > 0.).then_some(width / height)
}

fn image_height(width: f32, ratio: f32) -> f32 {
    (width / ratio).clamp(width * 0.28, width * 2.)
}

fn read_image_ratio(path: &Path) -> Option<f32> {
    let reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    (width > 0 && height > 0).then_some(width as f32 / height as f32)
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.)
    } else {
        tokens.to_string()
    }
}

fn done_footer(node: &BoardNode) -> String {
    let mut footer = format!(
        "✓ Finished{}",
        if node.images.len() > 1 {
            format!(" · {} images", node.images.len())
        } else {
            String::new()
        }
    );
    if !node.text.is_empty() {
        footer.push_str(" · ");
        footer.extend(node.text.chars().take(90));
    }
    if node.token_count() > 0 {
        footer.push_str(&format!(" · {} tok", format_tokens(node.token_count())));
    }
    footer
}

fn status_message(node: &BoardNode) -> String {
    let message = match node.status {
        NodeStatus::Error => node.error.as_deref().unwrap_or("Generation failed"),
        NodeStatus::Stopped => match node.stop_reason {
            Some(StopReason::User) => "Stopped by you.",
            Some(StopReason::AppQuit) => "Stopped when CodexImage quit.",
            Some(StopReason::Deleted) => "Stopped when this node was deleted.",
            None => "Stopped.",
        },
        NodeStatus::Running | NodeStatus::Done => "",
    };
    message
        .chars()
        .take(220)
        .map(|character| if character == '\n' { ' ' } else { character })
        .collect()
}

fn format_date(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp_millis(timestamp)
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%d.%m.%y")
                .to_string()
        })
        .unwrap_or_default()
}

fn time_ago(timestamp: i64) -> String {
    let seconds = ((now_ms() - timestamp) / 1_000).max(0);
    if seconds < 60 {
        "now".into()
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

fn status_label(node: &BoardNode) -> String {
    match node.status {
        NodeStatus::Running => "Generating".into(),
        NodeStatus::Error => "Failed".into(),
        NodeStatus::Stopped => "Stopped".into(),
        NodeStatus::Done => format!(
            "{} image{}",
            node.images.len(),
            if node.images.len() == 1 { "" } else { "s" }
        ),
    }
}

fn node_depths(board: &Board) -> HashMap<String, usize> {
    let by_id: HashMap<_, _> = board
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    board
        .nodes
        .iter()
        .map(|node| {
            let mut depth = 0;
            let mut current = node.parent_id.as_deref();
            let mut seen = HashSet::new();
            while let Some(id) = current {
                if !seen.insert(id) {
                    break;
                }
                let Some(parent) = by_id.get(id) else { break };
                depth += 1;
                current = parent.parent_id.as_deref();
            }
            (node.id.clone(), depth)
        })
        .collect()
}

fn image_format_for_path(path: &Path) -> Option<ImageFormat> {
    match path
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "webp" => Some(ImageFormat::Webp),
        "gif" => Some(ImageFormat::Gif),
        "svg" => Some(ImageFormat::Svg),
        "bmp" => Some(ImageFormat::Bmp),
        "tif" | "tiff" => Some(ImageFormat::Tiff),
        "ico" => Some(ImageFormat::Ico),
        "pnm" | "pbm" | "pgm" | "ppm" => Some(ImageFormat::Pnm),
        _ => None,
    }
}

#[cfg(test)]
mod card_layout_tests {
    use super::{
        CARD_WIDTH, CardColor, CardRect, CardScene, ConnectorStyle, DashCommand, GRID_COLOR_BGRA,
        GRID_GAP, GRID_TEXTURE_SCALE, GRID_TILE_SIZE, Lightbox, LightboxLocation, OutputLayout,
        PROMPT_WRAP_COLUMNS, card_height, card_scene_svg, dot_grid_metrics,
        dot_grid_texture_pixels, edge_is_visible, image_needs_high_resolution, lightbox_target,
        output_layout, prompt_metrics_from_line_count, rect_is_visible, trace_dashed_polyline,
        wrap_prompt,
    };
    use crate::model::{Board, BoardNode, NodeStatus};
    use gpui::{point, px};
    use std::collections::HashMap;
    use unicode_segmentation::UnicodeSegmentation;

    fn node(images: usize, status: NodeStatus) -> BoardNode {
        BoardNode {
            id: "node".into(),
            parent_id: None,
            prompt: "A concise prompt that should preserve its line shape while zooming".into(),
            aspect: "auto".into(),
            source_images: Vec::new(),
            attachments: Vec::new(),
            images: (0..images).map(|index| format!("/{index}.png")).collect(),
            image_labels: Vec::new(),
            attempts: Vec::new(),
            text: String::new(),
            status,
            error: None,
            stop_reason: None,
            x: None,
            y: None,
            created_at: 0,
            run_started_at: None,
            finished_at: None,
            usage: None,
        }
    }

    fn tree_node(id: &str, parent_id: Option<&str>, images: &[&str], created_at: i64) -> BoardNode {
        let mut node = node(0, NodeStatus::Done);
        node.id = id.into();
        node.parent_id = parent_id.map(str::to_owned);
        node.images = images.iter().map(|image| (*image).to_owned()).collect();
        node.created_at = created_at;
        node
    }

    #[test]
    fn lightbox_keeps_the_displayed_image_until_a_pending_image_is_committed() {
        let mut lightbox = Lightbox {
            node_id: "node-a".into(),
            image: "a.png".into(),
            zoom: 3.,
            pan_x: 40.,
            pan_y: -20.,
            pending: None,
        };
        let destination = LightboxLocation {
            node_id: "node-b".into(),
            image: "b.png".into(),
        };

        lightbox.request(destination.clone());
        assert_eq!(lightbox.displayed_location().image, "a.png");
        assert_eq!(lightbox.navigation_origin(), destination);
        assert_eq!(lightbox.zoom, 3.);

        assert!(lightbox.commit_pending(&destination));
        assert_eq!(lightbox.displayed_location(), destination);
        assert_eq!(lightbox.zoom, 1.);
        assert_eq!((lightbox.pan_x, lightbox.pan_y), (0., 0.));
        assert!(lightbox.pending.is_none());
    }

    #[test]
    fn lightbox_gesture_zoom_scales_and_clamps_safely() {
        let mut lightbox = Lightbox {
            node_id: "node-a".into(),
            image: "a.png".into(),
            zoom: 1.,
            pan_x: 0.,
            pan_y: 0.,
            pending: None,
        };

        lightbox.zoom_at(1.5, point(px(500.), px(400.)), 1_000., 800., 1.);
        assert_eq!(lightbox.zoom, 1.5);
        lightbox.zoom_at(0.5, point(px(500.), px(400.)), 1_000., 800., 1.);
        assert_eq!(lightbox.zoom, 1.);
        lightbox.zoom_at(20., point(px(500.), px(400.)), 1_000., 800., 1.);
        assert_eq!(lightbox.zoom, 8.);
        lightbox.zoom_at(f32::NAN, point(px(500.), px(400.)), 1_000., 800., 1.);
        assert_eq!(lightbox.zoom, 8.);
    }

    #[test]
    fn lightbox_zoom_preserves_the_image_point_beneath_the_gesture() {
        let mut lightbox = Lightbox {
            node_id: "node-a".into(),
            image: "a.png".into(),
            zoom: 1.,
            pan_x: 0.,
            pan_y: 0.,
            pending: None,
        };
        let focal_x = 200.;
        let image_point_before = (focal_x - lightbox.pan_x) / lightbox.zoom;

        lightbox.zoom_at(2., point(px(700.), px(400.)), 1_000., 800., 1.);

        let image_point_after = (focal_x - lightbox.pan_x) / lightbox.zoom;
        assert_eq!(lightbox.zoom, 2.);
        assert_eq!(lightbox.pan_x, -200.);
        assert_eq!(image_point_before, image_point_after);
    }

    #[test]
    fn lightbox_drag_pan_is_clamped_to_the_visible_image_bounds() {
        let mut lightbox = Lightbox {
            node_id: "node-a".into(),
            image: "a.png".into(),
            zoom: 2.,
            pan_x: 0.,
            pan_y: 0.,
            pending: None,
        };

        lightbox.pan_to(999., -999., 1_000., 800., 2.);

        assert_eq!(lightbox.pan_x, 500.);
        assert_eq!(lightbox.pan_y, -100.);
    }

    #[test]
    fn lightbox_navigation_resolves_destinations_without_changing_the_displayed_image() {
        let board = Board {
            id: "board".into(),
            title: "Board".into(),
            created_at: 0,
            nodes: vec![
                tree_node("root", None, &["root-0"], 0),
                tree_node("take-a", Some("root"), &["a-0", "a-1"], 1),
                tree_node("take-b", Some("root"), &["b-0", "b-1"], 2),
                tree_node("empty-child", Some("take-a"), &[], 3),
                tree_node("descendant", Some("empty-child"), &["d-0"], 4),
            ],
        };
        let a0 = LightboxLocation {
            node_id: "take-a".into(),
            image: "a-0".into(),
        };
        let a1 = lightbox_target(&board, &a0, 1, 0).expect("next image");
        assert_eq!(a1.node_id, "take-a");
        assert_eq!(a1.image, "a-1");

        let b0 = lightbox_target(&board, &a1, 1, 0).expect("next sibling");
        assert_eq!(b0.node_id, "take-b");
        assert_eq!(b0.image, "b-0");
        assert_eq!(lightbox_target(&board, &b0, -1, 0), Some(a1.clone()));

        let parent = lightbox_target(&board, &a0, 0, -1).expect("parent image");
        assert_eq!(parent.node_id, "root");
        assert_eq!(parent.image, "root-0");
        let descendant = lightbox_target(&board, &a0, 0, 1).expect("descendant image");
        assert_eq!(descendant.node_id, "descendant");
        assert_eq!(descendant.image, "d-0");
    }

    #[test]
    fn four_outputs_form_two_non_overlapping_columns() {
        let node = node(4, NodeStatus::Done);
        let ratios = HashMap::from([
            ("/0.png".into(), 1.0),
            ("/1.png".into(), 0.5),
            ("/2.png".into(), 2.0),
            ("/3.png".into(), 1.0),
        ]);
        let OutputLayout::Tiles { height, cells } = output_layout(&node, &ratios) else {
            panic!("expected tiled layout")
        };
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[0].x, 0.);
        assert!(cells[1].x > cells[0].x + cells[0].width);
        assert!(cells[2].y > cells[0].y);
        assert!(height >= cells[3].y + cells[3].height);
    }

    #[test]
    fn large_output_sets_use_a_bounded_hero_and_filmstrip() {
        let node = node(9, NodeStatus::Done);
        let OutputLayout::Filmstrip {
            hero_height,
            compact_count,
            hidden_count,
            strip_cell_width,
            ..
        } = output_layout(&node, &HashMap::new())
        else {
            panic!("expected filmstrip layout")
        };
        assert_eq!(compact_count, 4);
        assert_eq!(hidden_count, 4);
        assert!(hero_height <= CARD_WIDTH * 2.);
        assert!(strip_cell_width < CARD_WIDTH / 4.);
    }

    #[test]
    fn empty_error_state_reserves_more_space_than_a_done_footer() {
        let error = node(0, NodeStatus::Error);
        let done = node(0, NodeStatus::Done);
        assert!(
            card_height(&error, false, &HashMap::new())
                > card_height(&done, false, &HashMap::new())
        );
    }

    #[test]
    fn prompt_wrap_is_deterministic_and_grapheme_safe() {
        let prompt = "alpha beta supercalifragilistic 🌍🌎🌏🌐";
        let lines = wrap_prompt(prompt, 8);
        assert!(lines.iter().all(|line| line.graphemes(true).count() <= 8));
        let source = prompt
            .chars()
            .filter(|character| !character.is_whitespace());
        let wrapped = lines
            .iter()
            .flat_map(|line| line.chars())
            .filter(|character| !character.is_whitespace());
        assert_eq!(source.collect::<String>(), wrapped.collect::<String>());
    }

    #[test]
    fn prompt_wrap_preserves_explicit_blank_lines() {
        assert_eq!(
            wrap_prompt("one two\n\nthree", 6),
            vec!["one", "two", "", "three"]
        );
    }

    #[test]
    fn expanded_prompt_height_uses_the_same_world_space_lines() {
        let mut node = node(0, NodeStatus::Done);
        node.prompt = std::iter::repeat_n("stable", 80)
            .collect::<Vec<_>>()
            .join(" ");
        let total_lines = wrap_prompt(&node.prompt, PROMPT_WRAP_COLUMNS).len();
        let (collapsed_lines, clamped) = prompt_metrics_from_line_count(total_lines, false);
        let (expanded_lines, expanded_clamped) = prompt_metrics_from_line_count(total_lines, true);
        assert_eq!(collapsed_lines, 6.);
        assert!(expanded_lines > collapsed_lines);
        assert!(clamped && expanded_clamped);
    }

    #[test]
    fn viewport_culling_keeps_intersecting_cards_and_rejects_distant_ones() {
        assert!(rect_is_visible(-40., 40., 100., 100., 800., 600., 16.));
        assert!(rect_is_visible(790., 590., 30., 30., 800., 600., 16.));
        assert!(!rect_is_visible(-200., 40., 100., 100., 800., 600., 16.));
        assert!(!rect_is_visible(900., 40., 30., 30., 800., 600., 16.));
    }

    #[test]
    fn edge_culling_uses_the_full_connector_bounds() {
        assert!(edge_is_visible(
            point(px(-100.), px(300.)),
            point(px(900.), px(300.)),
            800.,
            600.,
            16.,
        ));
        assert!(!edge_is_visible(
            point(px(-200.), px(-100.)),
            point(px(-100.), px(-40.)),
            800.,
            600.,
            16.,
        ));
    }

    #[test]
    fn connector_style_scales_with_canvas_zoom() {
        assert_eq!(
            ConnectorStyle::for_zoom(1.),
            ConnectorStyle {
                stroke_width: 1.6,
                dash_length: 7.,
                gap_length: 5.,
            }
        );
        assert_eq!(
            ConnectorStyle::for_zoom(0.25),
            ConnectorStyle {
                stroke_width: 0.4,
                dash_length: 1.75,
                gap_length: 1.25,
            }
        );
    }

    #[test]
    fn connector_dash_remains_continuous_around_elbows() {
        let points = [
            point(px(0.), px(0.)),
            point(px(0.), px(4.)),
            point(px(4.), px(4.)),
        ];
        let mut commands = Vec::new();
        let style = ConnectorStyle::for_zoom(1.);
        trace_dashed_polyline(&points, style.dash_length, style.gap_length, |command| {
            commands.push(command)
        });

        assert_eq!(
            commands,
            vec![
                DashCommand::MoveTo(point(px(0.), px(0.))),
                DashCommand::LineTo(point(px(0.), px(4.))),
                DashCommand::LineTo(point(px(3.), px(4.))),
            ]
        );
    }

    #[test]
    fn sprite_tiers_preserve_one_world_space_scene() {
        let mut scene = CardScene {
            height: 510.,
            primitives: Vec::new(),
        };
        scene.quad(
            CardRect::new(0., 0., CARD_WIDTH, scene.height),
            20.,
            CardColor::Raised,
            Some((1., CardColor::Line)),
        );
        scene.text(
            "A <stable> & exact card",
            CardRect::new(14., 12., CARD_WIDTH - 28., 24.),
            14.,
            18.,
            CardColor::Ink,
            gpui::TextAlign::Left,
        );

        for width in super::CARD_SPRITE_WIDTHS {
            let svg = card_scene_svg(&scene, width);
            assert!(svg.contains("viewBox=\"0 0 340 510\""));
            assert!(svg.contains("A &lt;stable&gt; &amp; exact card"));
            assert!(svg.contains(&format!("width=\"{width}\"")));
        }
    }

    #[test]
    fn node_images_promote_to_originals_only_when_thumbnails_are_undersized() {
        let full_card_image = CardRect::new(0., 0., CARD_WIDTH, CARD_WIDTH);
        assert!(!image_needs_high_resolution(full_card_image, 1., 2.));
        assert!(image_needs_high_resolution(full_card_image, 1.1, 2.));

        let half_width_tile = CardRect::new(0., 0., 169., 169.);
        assert!(!image_needs_high_resolution(half_width_tile, 2., 2.));

        let portrait_hero = CardRect::new(0., 0., CARD_WIDTH, CARD_WIDTH * 2.);
        assert!(image_needs_high_resolution(portrait_hero, 0.6, 2.));
    }

    #[test]
    fn dot_grid_tile_is_world_anchored_at_every_zoom() {
        let grid = dot_grid_metrics(5., -3., 1.);
        assert!((grid.tile_size - GRID_TILE_SIZE).abs() < f32::EPSILON);
        let dot_phase_x = (grid.origin_x + GRID_GAP / 2.).rem_euclid(GRID_GAP);
        let dot_phase_y = (grid.origin_y + GRID_GAP / 2.).rem_euclid(GRID_GAP);
        assert!((dot_phase_x - 5.).abs() < f32::EPSILON);
        assert!((dot_phase_y - 25.).abs() < f32::EPSILON);

        let one_tile_right = dot_grid_metrics(5. + GRID_TILE_SIZE, -3., 1.);
        assert!((one_tile_right.origin_x - grid.origin_x).abs() < f32::EPSILON);

        let distant = dot_grid_metrics(5., -3., 0.08);
        assert!((distant.tile_size - GRID_TILE_SIZE * 0.08).abs() < f32::EPSILON);
        let distant_phase = (distant.origin_x + GRID_GAP * 0.08 / 2.).rem_euclid(GRID_GAP * 0.08);
        assert!((distant_phase - 5_f32.rem_euclid(GRID_GAP * 0.08)).abs() < 0.0001);
    }

    #[test]
    fn dot_grid_texture_matches_the_web_canvas_contract() {
        let texture = dot_grid_texture_pixels();
        let expected_size = (GRID_TILE_SIZE * GRID_TEXTURE_SCALE as f32) as u32;
        assert_eq!(texture.dimensions(), (expected_size, expected_size));
        assert_eq!(texture.get_pixel(0, 0).0, [0, 0, 0, 0]);

        let first_dot = texture.get_pixel(27, 27).0;
        assert_eq!(&first_dot[..3], &GRID_COLOR_BGRA);
        assert!(first_dot[3] > 200);

        let last_dot_center = ((GRID_TILE_SIZE - GRID_GAP / 2.) * GRID_TEXTURE_SCALE as f32) as u32;
        let last_dot = texture
            .get_pixel(last_dot_center - 1, last_dot_center - 1)
            .0;
        assert_eq!(&last_dot[..3], &GRID_COLOR_BGRA);
        assert!(last_dot[3] > 200);
    }
}
