//! The application window: board state, overlays, input handling, and the
//! element tree that hosts the painted canvas.

use super::canvas::CanvasConnector;
use super::canvas_view::{DragState, MinimapScene};
use super::card::{
    CanvasImageAsset, CanvasNode, PROMPT_WRAP_COLUMNS, card_height, card_height_from_metadata,
    output_layout, wrap_prompt,
};
use super::composer::ComposerTarget;
use super::format::read_image_ratio;
use super::image_cache::{
    CARD_SPRITE_CACHE_BUDGET, CardSpriteCache, DECODED_IMAGE_CACHE_BUDGET, DecodedImageCache,
};
use super::input::TextInput;
use super::keymap::{Escape, Generate, Quit, bind_keys, configure_menus};
use super::lightbox::Lightbox;
use super::overlays::{BoardRow, GalleryRow, Toast};
use super::theme;
#[cfg(target_os = "macos")]
use super::window_memory::{VisibilityChange, WindowMemoryState, WindowOcclusionObserver};
use crate::APP_NAME;
use crate::generation::GenerationEngine;
use crate::layout::{Position, compute_layout};
use crate::model::{Board, BoardNode};
use crate::storage::{
    RepositoryEvent, create_thumbnail, sprite_thumbnail_path_for, thumbnail_path_for,
};
use anyhow::{Context as _, Result};
use gpui::{
    App, AppContext, Bounds, Context, Entity, ExternalPaths, FocusHandle, Focusable, ListAlignment,
    ListState, MouseButton, Render, Role, SharedString, Subscription, Task, TitlebarOptions,
    Window, WindowBounds, WindowOptions, div, point, prelude::*, px, size,
};
use gpui_platform::application;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

pub(super) enum Overlay {
    None,
    Boards,
    Gallery,
    Lightbox(Lightbox),
    EditNode(String),
    RenameBoard(String),
    NodeText(String),
    QuitConfirm,
}

pub(super) struct ImageAsset {
    pub(super) original: PathBuf,
    pub(super) thumbnail: PathBuf,
    /// The tiny `s_` thumbnail, falling back to `thumbnail` until it exists.
    pub(super) sprite: PathBuf,
}

/// What one background image job resolved, applied on the main thread.
struct ResolvedImage {
    url: String,
    thumbnail: Option<PathBuf>,
    sprite: Option<PathBuf>,
    ratio: Option<f32>,
}

/// Thumbnail encoding and header reads for one image, performed off the render
/// thread so a finished generation never stalls the canvas.
#[derive(Clone)]
struct ImageJob {
    url: String,
    original: PathBuf,
    thumbnail: Option<PathBuf>,
    read_ratio: bool,
}

fn board_snapshots_equal(left: Option<&Arc<Board>>, right: Option<&Arc<Board>>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

pub(super) struct AppView {
    pub(super) engine: GenerationEngine,
    pub(super) receiver: async_channel::Receiver<RepositoryEvent>,
    pub(super) board_id: Option<String>,
    pub(super) board: Option<Arc<Board>>,
    pub(super) prompt: Entity<TextInput>,
    pub(super) modal_input: Entity<TextInput>,
    pub(super) search_input: Entity<TextInput>,
    pub(super) focus: FocusHandle,
    pub(super) lightbox_focus: FocusHandle,
    pub(super) overlay: Overlay,
    pub(super) target: Option<ComposerTarget>,
    pub(super) attachments: Vec<PathBuf>,
    pub(super) pending_attachment_writes: usize,
    pub(super) composer_submission_pending: bool,
    pub(super) aspect_index: usize,
    pub(super) count: usize,
    pub(super) activity: HashMap<String, String>,
    pub(super) hovered_node: Option<String>,
    pub(super) hovered_toolbar_button: Option<usize>,
    pub(super) expanded_prompts: HashSet<String>,
    pub(super) armed_board_delete: Option<String>,
    pub(super) toast: Option<Toast>,
    pub(super) toast_serial: u64,
    pub(super) toast_task: Option<Task<()>>,
    pub(super) layout: HashMap<String, Position>,
    pub(super) heights: HashMap<String, f32>,
    pub(super) canvas_nodes: Arc<Vec<Arc<CanvasNode>>>,
    pub(super) canvas_connectors: Arc<Vec<CanvasConnector>>,
    pub(super) minimap_scene: Option<MinimapScene>,
    /// Rows for whichever overlay is open. Building them walks every node of
    /// every board, so they are derived when the data changes rather than on
    /// every frame; a closed overlay's rows are stale and unread until it opens.
    pub(super) board_rows: Arc<Vec<BoardRow>>,
    pub(super) gallery_rows: Arc<Vec<GalleryRow>>,
    pub(super) image_cache: Entity<DecodedImageCache>,
    pub(super) sprite_cache: Entity<CardSpriteCache>,
    pub(super) gallery_list_state: ListState,
    pub(super) image_ratios: HashMap<String, f32>,
    pub(super) image_assets: HashMap<String, ImageAsset>,
    pending_image_jobs: HashSet<String>,
    image_metadata_tasks: Vec<Task<()>>,
    pub(super) transient_positions: HashMap<String, Position>,
    pub(super) camera_x: f32,
    pub(super) camera_y: f32,
    pub(super) zoom: f32,
    pub(super) zoom_settled: bool,
    pub(super) zoom_settle_task: Option<Task<()>>,
    pub(super) running_tick_task: Option<Task<()>>,
    repository_task: Option<Task<()>>,
    #[cfg(target_os = "macos")]
    window_memory_state: WindowMemoryState,
    #[cfg(target_os = "macos")]
    _window_occlusion_observer: Option<WindowOcclusionObserver>,
    #[cfg(target_os = "macos")]
    window_occlusion_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    pub(super) drag: Option<DragState>,
}

impl Drop for AppView {
    fn drop(&mut self) {
        let pending = self
            .engine
            .repository()
            .paths()
            .root
            .join("pending-attachments");
        for path in &self.attachments {
            if path.starts_with(&pending) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

pub fn run() -> Result<()> {
    let (sender, receiver) = async_channel::bounded(1_024);
    let repository = crate::storage::Repository::open(sender)?;
    super::disk_cache::init(repository.paths().root.join("decoded-cache"));
    // One sequential background pass archives and conditions legacy generated
    // images once, then catches up thumbnails created by older versions.
    let sweeper = repository.clone();
    std::thread::spawn(move || {
        sweeper.condition_existing_generated_images();
        sweeper.refresh_undersized_thumbnails();
    });
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
        #[cfg(not(target_os = "macos"))]
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
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
        let image_cache = cx.new(|cx| DecodedImageCache::new(DECODED_IMAGE_CACHE_BUDGET, cx));
        let sprite_cache = cx.new(|cx| CardSpriteCache::new(CARD_SPRITE_CACHE_BUDGET, cx));
        #[cfg(target_os = "macos")]
        let (window_occlusion_observer, window_occlusion_receiver) =
            match WindowOcclusionObserver::new(window) {
                Ok((observer, receiver)) => (Some(observer), Some(receiver)),
                Err(error) => {
                    eprintln!("Could not observe window occlusion: {error:#}");
                    (None, None)
                }
            };
        // Activation follows keyboard focus, not visibility. It only controls
        // animation ticks; true occlusion owns the GPU cache lifetime below.
        let activation_subscription = cx.observe_window_activation(window, |view, window, cx| {
            if window.is_window_active() {
                // Restart a visible running animation after the window becomes
                // active. `render_canvas` decides whether a tick is needed.
                cx.notify();
            } else {
                view.running_tick_task.take();
            }
        });
        let prompt_subscription = cx.subscribe(&prompt, |this, _, event, cx| {
            this.handle_input_event(event, cx)
        });
        let search_subscription = cx.observe(&search_input, |_, _, cx| cx.notify());
        let summaries = engine.repository().summaries();
        let board_id = summaries.first().map(|summary| summary.id.clone());
        let board = board_id
            .as_deref()
            .and_then(|id| engine.repository().board_snapshot(id));
        let gallery_list_state = ListState::new(
            board.as_ref().map_or(0, |board| board.nodes.len()),
            ListAlignment::Top,
            px(600.),
        );
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
            pending_attachment_writes: 0,
            composer_submission_pending: false,
            aspect_index: 0,
            count: 1,
            activity: HashMap::new(),
            hovered_node: None,
            hovered_toolbar_button: None,
            expanded_prompts: HashSet::new(),
            armed_board_delete: None,
            toast: None,
            toast_serial: 0,
            toast_task: None,
            layout: HashMap::new(),
            heights: HashMap::new(),
            canvas_nodes: Arc::new(Vec::new()),
            canvas_connectors: Arc::new(Vec::new()),
            minimap_scene: None,
            board_rows: Arc::new(Vec::new()),
            gallery_rows: Arc::new(Vec::new()),
            image_cache,
            sprite_cache,
            gallery_list_state,
            image_ratios: HashMap::new(),
            image_assets: HashMap::new(),
            pending_image_jobs: HashSet::new(),
            image_metadata_tasks: Vec::new(),
            transient_positions: HashMap::new(),
            camera_x: 80.,
            camera_y: 90.,
            zoom: 1.,
            zoom_settled: true,
            zoom_settle_task: None,
            running_tick_task: None,
            repository_task: None,
            #[cfg(target_os = "macos")]
            window_memory_state: WindowMemoryState::default(),
            #[cfg(target_os = "macos")]
            _window_occlusion_observer: window_occlusion_observer,
            #[cfg(target_os = "macos")]
            window_occlusion_task: None,
            _subscriptions: vec![
                activation_subscription,
                prompt_subscription,
                search_subscription,
            ],
            drag: None,
        };
        view.refresh_image_metadata(cx);
        view.refresh_layout();
        let receiver = view.receiver.clone();
        let window_handle = window.window_handle();
        view.repository_task = Some(cx.spawn(async move |weak, cx| {
            while let Ok(event) = receiver.recv().await {
                let mut events = Vec::with_capacity(receiver.len().saturating_add(1).min(256));
                events.push(event);
                while let Ok(event) = receiver.try_recv() {
                    events.push(event);
                }
                if window_handle
                    .update(cx, |_, window, cx| {
                        weak.update(cx, |view, cx| {
                            view.handle_repository_events(events, window, cx)
                        })
                    })
                    .and_then(|result| result)
                    .is_err()
                {
                    break;
                }
            }
        }));
        #[cfg(target_os = "macos")]
        if let Some(receiver) = window_occlusion_receiver {
            let window_handle = window.window_handle();
            view.window_occlusion_task = Some(cx.spawn(async move |weak, cx| {
                while let Ok(visible) = receiver.recv().await {
                    if window_handle
                        .update(cx, |_, window, cx| {
                            weak.update(cx, |view, cx| {
                                view.handle_window_visibility(visible, window, cx)
                            })
                        })
                        .and_then(|result| result)
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
        window.focus(&view.focus, cx);
        view
    }

    #[cfg(target_os = "macos")]
    fn handle_window_visibility(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.window_memory_state.update(visible) {
            VisibilityChange::None => {}
            VisibilityChange::Release => {
                self.running_tick_task.take();
                let images_released = self
                    .image_cache
                    .update(cx, |cache, cx| cache.clear(window, cx));
                let sprites_released = self
                    .sprite_cache
                    .update(cx, |cache, cx| cache.clear(window, cx));
                if images_released || sprites_released {
                    // Invalidate the retained scene so it cannot keep using
                    // atlas entries removed by either cache.
                    window.refresh();
                }
            }
            VisibilityChange::Restore => {
                // Images and sprites repopulate on demand in the next render.
                cx.notify();
                window.refresh();
            }
        }
    }

    fn handle_repository_events(
        &mut self,
        events: Vec<RepositoryEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        let mut images_rewritten = false;
        let mut visual_change = false;
        for event in events {
            match event {
                RepositoryEvent::Changed => changed = true,
                RepositoryEvent::ImagesRewritten => images_rewritten = true,
                RepositoryEvent::Activity { node_id, text } => {
                    if self.activity.get(&node_id) != Some(&text) {
                        self.activity.insert(node_id, text);
                        visual_change = true;
                    }
                }
                RepositoryEvent::PersistFailed(message) => self.show_toast(
                    format!("Could not save this board: {message}"),
                    true,
                    None,
                    cx,
                ),
            }
        }

        if images_rewritten {
            self.clear_render_caches(window, cx);
            self.reset_image_metadata();
            changed = true;
        }
        if changed {
            let next_board = self
                .board_id
                .as_deref()
                .and_then(|id| self.engine.repository().board_snapshot(id));
            let board_changed = !board_snapshots_equal(self.board.as_ref(), next_board.as_ref());
            if board_changed || images_rewritten {
                self.board = next_board;
                self.refresh_image_metadata(cx);
                self.refresh_layout();
                visual_change = true;
            } else if matches!(self.overlay, Overlay::Boards) {
                // A different board changed. Only the switcher's summaries can
                // have changed; the open canvas remains byte-for-byte equal.
                self.refresh_overlay_data();
                visual_change = true;
            }
        }
        if visual_change
            && (changed || !matches!(self.overlay, Overlay::Gallery | Overlay::Lightbox(_)))
        {
            cx.notify();
        }
    }

    /// Rebuilds the derived board state. Cards whose node, layout, and images
    /// are all unchanged are carried over wholesale, so a single finished
    /// generation no longer re-wraps every prompt or re-encodes every sprite.
    pub(super) fn refresh_layout(&mut self) {
        let previous = std::mem::take(&mut self.canvas_nodes);
        let cached: HashMap<&str, &Arc<CanvasNode>> = previous
            .iter()
            .map(|canvas_node| (canvas_node.node.id.as_str(), canvas_node))
            .collect();
        if let Some(board) = &self.board {
            // Keep transient derivation index-aligned with the board. The old
            // string-keyed prompt/output maps cloned every node id and every
            // wrapped-line Vec even when a repository event changed one node.
            let mut derived = Vec::with_capacity(board.nodes.len());
            let mut heights = HashMap::with_capacity(board.nodes.len());
            for node in &board.nodes {
                let previous_with_same_prompt = cached
                    .get(node.id.as_str())
                    .copied()
                    .filter(|previous| previous.node.prompt == node.prompt);
                let prompt_lines = previous_with_same_prompt.is_none().then(|| {
                    wrap_prompt(&node.prompt, PROMPT_WRAP_COLUMNS)
                        .into_iter()
                        .map(SharedString::from)
                        .collect::<Vec<_>>()
                });
                let prompt_line_count = prompt_lines.as_ref().map_or_else(
                    || previous_with_same_prompt.map_or(1, |previous| previous.prompt_lines.len()),
                    Vec::len,
                );
                let output_layout = output_layout(node, &self.image_ratios);
                let height = card_height_from_metadata(
                    node,
                    self.expanded_prompts.contains(&node.id),
                    prompt_line_count,
                    output_layout.height(),
                );
                heights.insert(node.id.clone(), height);
                derived.push((prompt_lines, output_layout));
            }
            let layout = compute_layout(&board.nodes, &heights);
            let canvas_nodes = board
                .nodes
                .iter()
                .zip(derived)
                .map(|(node, (prompt_lines, output_layout))| {
                    let expanded = self.expanded_prompts.contains(&node.id);
                    if let Some(reusable) = cached.get(node.id.as_str()).filter(|previous| {
                        previous.expanded == expanded
                            && previous.output_layout == output_layout
                            && previous.node == *node
                            && self.canvas_node_assets_are_current(previous)
                    }) {
                        return Arc::clone(reusable);
                    }
                    let prompt_lines = prompt_lines.unwrap_or_else(|| {
                        cached
                            .get(node.id.as_str())
                            .map(|previous| previous.prompt_lines.clone())
                            .unwrap_or_default()
                    });
                    Arc::new(CanvasNode::build(
                        node,
                        prompt_lines,
                        output_layout,
                        expanded,
                        |url| self.canvas_image_asset(url),
                    ))
                })
                .collect();
            self.canvas_nodes = Arc::new(canvas_nodes);
            self.heights = heights;
            self.layout = layout;
        } else {
            self.layout.clear();
            self.heights.clear();
            self.canvas_nodes = Arc::new(Vec::new());
        }
        self.refresh_canvas_scene();
        self.refresh_overlay_data();
    }

    /// Rebuilds the rows the open overlay renders from. Called whenever the
    /// derived board state changes and whenever an overlay opens, so the
    /// render pass itself stays a read of already-built rows.
    pub(super) fn refresh_overlay_data(&mut self) {
        match self.overlay {
            Overlay::Boards => {
                self.board_rows = Arc::new(
                    self.engine
                        .repository()
                        .summaries()
                        .into_iter()
                        .map(|summary| BoardRow::new(summary, self))
                        .collect(),
                );
            }
            Overlay::Gallery => {
                self.gallery_rows = Arc::new(GalleryRow::rows_for(self));
                // The list indexes straight into these rows, so its item count
                // may only ever change together with them.
                if self.gallery_list_state.item_count() == self.gallery_rows.len() {
                    self.gallery_list_state.remeasure();
                } else {
                    self.gallery_list_state.reset(self.gallery_rows.len());
                }
            }
            _ => {}
        }
    }

    /// Resolves each image to the file the canvas should draw. Anything that
    /// requires decoding — building a missing thumbnail, reading an aspect
    /// ratio — is handed to the background executor and applied when it lands.
    pub(super) fn reset_image_metadata(&mut self) {
        self.image_metadata_tasks.clear();
        self.pending_image_jobs.clear();
        self.image_assets.clear();
        self.image_ratios.clear();
    }

    pub(super) fn refresh_image_metadata(&mut self, cx: &mut Context<Self>) {
        let (Some(board_id), Some(board)) = (self.board_id.as_deref(), self.board.as_ref()) else {
            self.image_metadata_tasks.clear();
            self.image_assets.clear();
            self.image_ratios.clear();
            self.pending_image_jobs.clear();
            return;
        };
        let repository = self.engine.repository();
        let mut jobs = Vec::new();
        // Borrow the board's URLs while reconciling instead of cloning every
        // string into a temporary set on each repository update.
        let mut seen: HashSet<&str> = HashSet::new();
        for url in board.nodes.iter().flat_map(|node| {
            node.images
                .iter()
                .chain(&node.attempts)
                .chain(&node.attachments)
                .chain(&node.source_images)
        }) {
            if !seen.insert(url.as_str()) {
                continue;
            }
            // Existing assets were already resolved by a background image job.
            // Keeping them in place avoids rebuilding the PathBuf-heavy map and
            // rechecking missing sprite files on every repository update.
            if self.image_assets.contains_key(url) {
                continue;
            }
            let Some(original) = repository.image_path(board_id, url) else {
                continue;
            };
            let thumbnail_path = repository.thumbnail_path(board_id, url);
            let ready_thumbnail = thumbnail_path
                .as_ref()
                .filter(|path| path.exists())
                .cloned();
            let ready_sprite = repository
                .sprite_thumbnail_path(board_id, url)
                .filter(|path| path.exists());
            let read_ratio = !self.image_ratios.contains_key(url);
            if (ready_thumbnail.is_none() || read_ratio) && !self.pending_image_jobs.contains(url) {
                self.pending_image_jobs.insert(url.clone());
                jobs.push(ImageJob {
                    url: url.clone(),
                    original: original.clone(),
                    thumbnail: thumbnail_path,
                    read_ratio,
                });
            }
            let thumbnail = ready_thumbnail.unwrap_or_else(|| original.clone());
            self.image_assets.insert(
                url.clone(),
                ImageAsset {
                    sprite: ready_sprite.unwrap_or_else(|| thumbnail.clone()),
                    thumbnail,
                    original,
                },
            );
        }
        self.image_assets
            .retain(|url, _| seen.contains(url.as_str()));
        self.image_ratios
            .retain(|url, _| seen.contains(url.as_str()));
        self.pending_image_jobs
            .retain(|url| seen.contains(url.as_str()));
        drop(seen);
        self.spawn_image_jobs(board_id.to_owned(), jobs, cx);
    }

    /// Decodes in batches so a board full of new images costs one layout pass
    /// per batch rather than one per image, while still filling in progressively.
    fn spawn_image_jobs(&mut self, board_id: String, jobs: Vec<ImageJob>, cx: &mut Context<Self>) {
        const BATCH: usize = 64;
        if jobs.is_empty() {
            return;
        }
        self.image_metadata_tasks.retain(|task| !task.is_ready());
        let task = cx.spawn(async move |weak, cx| {
            for batch in jobs.chunks(BATCH) {
                let batch = batch.to_vec();
                let resolved = smol::unblock(move || {
                    batch
                        .into_iter()
                        .map(|job| {
                            let mut thumbnail = job.thumbnail.filter(|path| path.exists());
                            if thumbnail.is_none() && create_thumbnail(&job.original).is_ok() {
                                thumbnail =
                                    thumbnail_path_for(&job.original).filter(|path| path.exists());
                            }
                            let sprite = sprite_thumbnail_path_for(&job.original)
                                .filter(|path| path.exists());
                            let ratio = job
                                .read_ratio
                                .then(|| {
                                    read_image_ratio(thumbnail.as_deref().unwrap_or(&job.original))
                                })
                                .flatten();
                            ResolvedImage {
                                url: job.url,
                                thumbnail,
                                sprite,
                                ratio,
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .await;
                if weak
                    .update(cx, |view, cx| {
                        view.apply_image_metadata(&board_id, resolved, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        self.image_metadata_tasks.push(task);
    }

    fn apply_image_metadata(
        &mut self,
        board_id: &str,
        resolved: Vec<ResolvedImage>,
        cx: &mut Context<Self>,
    ) {
        if self.board_id.as_deref() != Some(board_id) {
            return;
        }
        let mut assets_changed = false;
        let mut layout_changed = false;
        for image in resolved {
            self.pending_image_jobs.remove(&image.url);
            let Some(asset) = self.image_assets.get_mut(&image.url) else {
                // The URL was removed while its background job was running.
                // Do not resurrect stale ratios or paths into the open board.
                continue;
            };
            if let Some(thumbnail) = image.thumbnail
                && asset.thumbnail != thumbnail
            {
                asset.thumbnail = thumbnail;
                assets_changed = true;
            }
            let sprite = image.sprite.unwrap_or_else(|| asset.thumbnail.clone());
            if asset.sprite != sprite {
                asset.sprite = sprite;
                assets_changed = true;
            }
            if let Some(ratio) = image.ratio
                && self.image_ratios.insert(image.url, ratio) != Some(ratio)
            {
                layout_changed = true;
            }
        }
        if layout_changed {
            self.refresh_layout();
            cx.notify();
        } else if assets_changed {
            self.refresh_canvas_assets();
            cx.notify();
        }
    }

    /// Rebuilds only cards whose resolved file paths changed. Thumbnail and
    /// sprite arrivals do not affect card heights or tree placement, so a full
    /// layout/minimap/connector pass here only multiplied startup work.
    fn refresh_canvas_assets(&mut self) {
        let Some(board) = &self.board else { return };
        let previous = std::mem::take(&mut self.canvas_nodes);
        let cached: HashMap<&str, &Arc<CanvasNode>> = previous
            .iter()
            .map(|canvas_node| (canvas_node.node.id.as_str(), canvas_node))
            .collect();
        let canvas_nodes = board
            .nodes
            .iter()
            .map(|node| {
                let previous = cached.get(node.id.as_str()).copied();
                if let Some(previous) = previous
                    && self.canvas_node_assets_are_current(previous)
                {
                    return Arc::clone(previous);
                }
                let prompt_lines = previous
                    .filter(|previous| previous.node.prompt == node.prompt)
                    .map(|previous| previous.prompt_lines.clone())
                    .unwrap_or_else(|| {
                        wrap_prompt(&node.prompt, PROMPT_WRAP_COLUMNS)
                            .into_iter()
                            .map(SharedString::from)
                            .collect()
                    });
                let output_layout = previous
                    .map(|previous| previous.output_layout.clone())
                    .unwrap_or_else(|| output_layout(node, &self.image_ratios));
                Arc::new(CanvasNode::build(
                    node,
                    prompt_lines,
                    output_layout,
                    self.expanded_prompts.contains(&node.id),
                    |url| self.canvas_image_asset(url),
                ))
            })
            .collect();
        self.canvas_nodes = Arc::new(canvas_nodes);
        self.refresh_overlay_data();
    }

    /// Whether a cached card still points at the files the app would resolve
    /// today; a background thumbnail landing invalidates only the cards using it.
    fn canvas_node_assets_are_current(&self, canvas_node: &CanvasNode) -> bool {
        let unchanged = |url: &str, used: &CanvasImageAsset| {
            self.image_assets.get(url).is_none_or(|current| {
                current.thumbnail.as_path() == used.thumbnail.as_ref()
                    && current.original.as_path() == used.original.as_ref()
                    && current.sprite.as_path() == used.sprite.as_ref()
            })
        };
        canvas_node
            .displayed_images
            .iter()
            .all(|image| unchanged(&image.url, &image.asset))
            && canvas_node
                .node
                .attachments
                .iter()
                .zip(&canvas_node.attachment_images)
                .all(|(url, asset)| unchanged(url, asset))
    }

    pub(super) fn card_height(&self, node: &BoardNode) -> f32 {
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

    pub(super) fn current_position(&self, id: &str) -> Option<Position> {
        self.transient_positions
            .get(id)
            .copied()
            .or_else(|| self.layout.get(id).copied())
    }

    pub(super) fn board_id(&self) -> Result<&str> {
        self.board_id.as_deref().context("No board is open")
    }

    pub(super) fn ensure_board(&mut self) -> Result<String> {
        if let Some(id) = &self.board_id {
            return Ok(id.clone());
        }
        let board = self.engine.repository().create_board()?;
        self.board_id = Some(board.id.clone());
        self.board = self.engine.repository().board_snapshot(&board.id);
        Ok(board.id)
    }

    pub(super) fn open_board(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_render_caches(window, cx);
        self.reset_image_metadata();
        self.board_id = Some(id.clone());
        self.board = self.engine.repository().board_snapshot(&id);
        self.overlay = Overlay::None;
        self.target = None;
        self.expanded_prompts.clear();
        self.transient_positions.clear();
        self.camera_x = 80.;
        self.camera_y = 90.;
        self.zoom = 1.;
        self.zoom_settled = true;
        self.zoom_settle_task.take();
        self.refresh_image_metadata(cx);
        self.refresh_layout();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn clear_render_caches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.image_cache
            .update(cx, |cache, cx| cache.clear(window, cx));
        self.sprite_cache
            .update(cx, |cache, cx| cache.clear(window, cx));
    }

    /// Enter submits whichever surface is open: the lightbox's quick-continue
    /// field, a modal, or the composer.
    pub(super) fn generate(&mut self, _: &Generate, window: &mut Window, cx: &mut Context<Self>) {
        match &self.overlay {
            Overlay::Lightbox(_) => self.continue_from_lightbox(window, cx),
            Overlay::EditNode(_) => self.save_edited_prompt(window, cx),
            Overlay::RenameBoard(_) => self.rename_open_board(window, cx),
            Overlay::Boards | Overlay::Gallery | Overlay::NodeText(_) | Overlay::QuitConfirm => {}
            Overlay::None => self.generate_from_composer(window, cx),
        }
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

    fn quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        if self.engine.active_count() > 0 {
            self.overlay = Overlay::QuitConfirm;
            cx.notify();
        } else {
            cx.quit();
        }
    }

    pub(super) fn node(&self, id: &str) -> Option<BoardNode> {
        self.board
            .as_ref()?
            .nodes
            .iter()
            .find(|node| node.id == id)
            .cloned()
    }

    pub(super) fn display_image_path(&self, url: &str, high_res: bool) -> PathBuf {
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
        let sprite = self
            .image_assets
            .get(url)
            .map(|asset| asset.sprite.clone())
            .unwrap_or_else(|| self.display_image_path(url, false));
        CanvasImageAsset {
            original: Arc::from(self.display_image_path(url, true)),
            thumbnail: Arc::from(self.display_image_path(url, false)),
            sprite: Arc::from(sprite),
        }
    }
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
        let overlay_covers_canvas = matches!(self.overlay, Overlay::Gallery | Overlay::Lightbox(_));
        if empty || overlay_covers_canvas {
            self.sync_running_tick(false, cx);
        }
        let mut root = div()
            .image_cache(self.image_cache.clone())
            .id("codex-image-app")
            .accessibility_id("codex-image.application")
            .key_context("CodexImage")
            .role(Role::Application)
            .aria_label(APP_NAME)
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
            .on_action(cx.listener(Self::reset_zoom))
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

        if !overlay_covers_canvas {
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
        }
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
            Overlay::NodeText(node_id) => {
                let node_id = node_id.clone();
                root.child(self.render_node_text(&node_id, cx))
            }
            Overlay::QuitConfirm => root.child(self.render_quit_confirm(cx)),
        };
        if let Some(toast) = &self.toast {
            root = root.child(self.render_toast(toast, window, cx));
        }
        root
    }
}
