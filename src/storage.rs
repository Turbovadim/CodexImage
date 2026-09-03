mod writer;

use crate::model::{
    Board, BoardNode, BoardSummary, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENT_TOTAL_BYTES,
    MAX_ATTACHMENTS, NewNodesRequest, NodeStatus, StopReason,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

const TRASH_TTL: Duration = Duration::from_secs(5 * 60);
// Sized so a portrait card at zoom 1 on a retina display (~1020 physical px)
// is served entirely by its thumbnail; full originals only decode when the
// user actually zooms in past that.
pub const THUMBNAIL_MAX_DIMENSION: u32 = 1080;
// A second, tiny thumbnail (`s_`) embedded by the small card-sprite tiers and
// painted at far-out zoom, where the media area is at most ~320 physical px.
// Rasterizing hundreds of cards during a zoom-out then decodes ~40 KB files
// instead of the full-size thumbnails.
pub const SPRITE_THUMBNAIL_MAX_DIMENSION: u32 = 320;
const THUMBNAIL_SWEEP_VERSION: u8 = 1;

#[derive(Clone, Debug)]
pub enum RepositoryEvent {
    Changed,
    ImagesRewritten,
    Activity {
        node_id: String,
        text: String,
    },
    /// Saving the board list failed. Emitted by the background writer, which
    /// has no caller left to return an error to.
    PersistFailed(String),
}

#[derive(Clone)]
pub struct Repository {
    inner: Arc<RwLock<RepositoryState>>,
    paths: Arc<DataPaths>,
    events: async_channel::Sender<RepositoryEvent>,
    writer: Arc<writer::BoardWriter>,
    cleanup: Arc<writer::CleanupWorker>,
}

#[derive(Debug)]
struct RepositoryState {
    // Boards are copy-on-write so the background writer can snapshot a few
    // cheap handles and serialize without holding the repository lock. A
    // mutation only clones its board when a write of the previous revision is
    // still in flight.
    boards: Vec<Arc<Board>>,
    trash: HashMap<String, TrashEntry>,
}

#[derive(Debug)]
struct TrashEntry {
    board_id: String,
    nodes: Vec<BoardNode>,
    expires_at: Instant,
}

#[derive(Debug)]
pub struct DataPaths {
    pub root: PathBuf,
    pub boards_file: PathBuf,
    pub images: PathBuf,
    pub generated_originals: PathBuf,
    pub workspaces: PathBuf,
    pub logs: PathBuf,
    pub output_schema: PathBuf,
    pub generated_images: PathBuf,
}

impl DataPaths {
    fn discover() -> Result<Self> {
        let root = std::env::var_os("CODEXIMAGE_DATA")
            .map(PathBuf::from)
            .or_else(crate::platform::data_root)
            .context("could not determine the CodexImage data directory")?;
        let generated_images = std::env::var_os("CODEXIMAGE_GENERATED_IMAGES")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".codex/generated_images")))
            .context("could not determine the Codex generated-images directory")?;
        Ok(Self::at(root, generated_images))
    }

    pub fn at(root: PathBuf, generated_images: PathBuf) -> Self {
        Self {
            boards_file: root.join("boards.json"),
            images: root.join("images"),
            generated_originals: root.join("generated-originals"),
            workspaces: root.join("workspaces"),
            logs: root.join("logs"),
            output_schema: root.join("output-manifest.schema.json"),
            root,
            generated_images,
        }
    }
}

impl Repository {
    pub fn open(events: async_channel::Sender<RepositoryEvent>) -> Result<Self> {
        Self::open_at(DataPaths::discover()?, events)
    }

    pub fn open_at(
        paths: DataPaths,
        events: async_channel::Sender<RepositoryEvent>,
    ) -> Result<Self> {
        for directory in [
            &paths.root,
            &paths.images,
            &paths.generated_originals,
            &paths.workspaces,
            &paths.logs,
        ] {
            fs::create_dir_all(directory)
                .with_context(|| format!("failed to create {}", directory.display()))?;
        }
        write_if_changed(
            &paths.output_schema,
            serde_json::to_vec_pretty(&crate::manifest::schema())?.as_slice(),
        )?;

        let mut boards = load_boards(&paths.boards_file)?;
        let mut changed = false;
        for board in &mut boards {
            for node in &mut board.nodes {
                if node.attempts.is_empty() && !node.images.is_empty() {
                    node.attempts.clone_from(&node.images);
                    changed = true;
                }
                if node.image_labels.len() != node.images.len() {
                    node.image_labels = node
                        .images
                        .iter()
                        .enumerate()
                        .map(|(index, _)| format!("Output {}", index + 1))
                        .collect();
                    changed = true;
                }
                if node.status == NodeStatus::Running {
                    node.status = NodeStatus::Error;
                    node.error = Some(
                        "Generation was interrupted because CodexImage closed unexpectedly.".into(),
                    );
                    node.finished_at = Some(now_ms());
                    changed = true;
                }
            }
        }
        let paths = Arc::new(paths);
        // The startup migration runs before the app can observe anything, so
        // it writes inline; every later save goes through the writer thread.
        if changed {
            write_boards(&paths.boards_file, &boards)?;
        }
        let state = Arc::new(RwLock::new(RepositoryState {
            boards: boards.into_iter().map(Arc::new).collect(),
            trash: HashMap::new(),
        }));
        let writer = Arc::new(writer::BoardWriter::spawn(
            Arc::clone(&state),
            paths.boards_file.clone(),
            events.clone(),
        ));
        let cleanup = Arc::new(
            writer::CleanupWorker::spawn().context("failed to start storage cleanup worker")?,
        );
        Ok(Self {
            inner: state,
            paths,
            events,
            writer,
            cleanup,
        })
    }

    pub fn paths(&self) -> &DataPaths {
        &self.paths
    }

    pub fn boards(&self) -> Vec<Board> {
        self.inner
            .read()
            .boards
            .iter()
            .map(|board| board.as_ref().clone())
            .collect()
    }

    pub fn board(&self, board_id: &str) -> Option<Board> {
        self.board_snapshot(board_id)
            .map(|board| board.as_ref().clone())
    }

    /// Cheap immutable board snapshot for background generation work.
    pub(crate) fn board_snapshot(&self, board_id: &str) -> Option<Arc<Board>> {
        self.inner
            .read()
            .boards
            .iter()
            .find(|board| board.id == board_id)
            .cloned()
    }

    pub fn node(&self, board_id: &str, node_id: &str) -> Option<BoardNode> {
        self.inner
            .read()
            .boards
            .iter()
            .find(|board| board.id == board_id)?
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .cloned()
    }

    pub fn summaries(&self) -> Vec<BoardSummary> {
        let mut summaries: Vec<_> = self
            .inner
            .read()
            .boards
            .iter()
            .map(|board| {
                let mut updated_at = None;
                let mut image_count = 0;
                let mut last_image = None;
                let mut total_tokens = 0;
                for node in &board.nodes {
                    updated_at = Some(updated_at.map_or(node.created_at, |updated_at: i64| {
                        updated_at.max(node.created_at)
                    }));
                    image_count += node.images.len();
                    if let Some(image) = node.images.last() {
                        last_image = Some(image.clone());
                    }
                    total_tokens += node.token_count();
                }
                BoardSummary {
                    id: board.id.clone(),
                    title: board.title.clone(),
                    created_at: board.created_at,
                    updated_at: updated_at.unwrap_or(board.created_at),
                    image_count,
                    last_image,
                    total_tokens,
                }
            })
            .collect();
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at));
        summaries
    }

    pub fn create_board(&self) -> Result<Board> {
        let board = Board {
            id: Uuid::new_v4().to_string(),
            title: "New board".into(),
            created_at: now_ms(),
            nodes: Vec::new(),
        };
        self.inner.write().boards.push(Arc::new(board.clone()));
        self.persist_and_notify();
        Ok(board)
    }

    pub fn rename_board(&self, board_id: &str, title: &str) -> Result<()> {
        let title = title.trim().chars().take(120).collect::<String>();
        if title.is_empty() {
            return Ok(());
        }
        let mut state = self.inner.write();
        board_mut(&mut state, board_id)?.title = title;
        drop(state);
        self.persist_and_notify();
        Ok(())
    }

    pub fn delete_board(&self, board_id: &str) -> Result<()> {
        let mut state = self.inner.write();
        let before = state.boards.len();
        state.boards.retain(|board| board.id != board_id);
        if state.boards.len() == before {
            bail!("Board not found");
        }
        state.trash.retain(|_, entry| entry.board_id != board_id);
        drop(state);
        self.persist_and_notify();
        let directories = [
            self.paths.images.join(board_id),
            self.paths.generated_originals.join(board_id),
            self.paths.workspaces.join(board_id),
        ];
        let log = self.paths.logs.join(format!("{board_id}.jsonl"));
        self.cleanup.request(directories, log);
        Ok(())
    }

    pub fn add_nodes(&self, board_id: &str, request: NewNodesRequest) -> Result<Vec<BoardNode>> {
        let prompt = request.prompt.trim().to_owned();
        if prompt.is_empty() {
            bail!("Empty prompt");
        }
        let count = request.count.clamp(1, 4);
        if request.attachment_paths.len() + request.attachment_urls.len() > MAX_ATTACHMENTS {
            bail!("Too many attachments (max {MAX_ATTACHMENTS})");
        }
        let parent = {
            let state = self.inner.read();
            let board = state
                .boards
                .iter()
                .find(|board| board.id == board_id)
                .context("Board not found")?;
            request
                .parent_id
                .as_deref()
                .map(|parent_id| {
                    board
                        .nodes
                        .iter()
                        .find(|node| node.id == parent_id)
                        .cloned()
                        .context("Parent node not found")
                })
                .transpose()?
        };
        let source_images = match (&parent, request.source_images) {
            (Some(_), Some(images)) => images,
            (Some(parent), None) => parent.images.clone(),
            (None, _) => Vec::new(),
        }
        .into_iter()
        .filter(|url| self.image_path(board_id, url).is_some())
        .collect::<Vec<_>>();

        let mut attachments = self.import_attachments(board_id, &request.attachment_paths)?;
        attachments.extend(
            request
                .attachment_urls
                .into_iter()
                .filter(|url| self.image_path(board_id, url).is_some()),
        );

        let run_started_at = now_ms();
        let nodes: Vec<_> = (0..count)
            .map(|index| BoardNode {
                id: Uuid::new_v4().to_string(),
                parent_id: parent.as_ref().map(|parent| parent.id.clone()),
                prompt: prompt.clone(),
                aspect: if request.aspect.is_empty() {
                    "auto".into()
                } else {
                    request.aspect.clone()
                },
                source_images: source_images.clone(),
                attachments: attachments.clone(),
                images: Vec::new(),
                image_labels: Vec::new(),
                attempts: Vec::new(),
                text: String::new(),
                status: NodeStatus::Running,
                error: None,
                stop_reason: None,
                x: request.position.map(|(x, _)| x + 32.0 * index as f32),
                y: request.position.map(|(_, y)| y + 32.0 * index as f32),
                created_at: run_started_at + index as i64,
                run_started_at: Some(run_started_at),
                finished_at: None,
                usage: None,
            })
            .collect();
        let mut state = self.inner.write();
        let board = board_mut(&mut state, board_id)?;
        if board.nodes.is_empty() {
            board.title = prompt.chars().take(60).collect();
        }
        board.nodes.extend(nodes.clone());
        drop(state);
        self.persist_and_notify();
        Ok(nodes)
    }

    pub fn regenerate_node(
        &self,
        board_id: &str,
        node_id: &str,
        prompt: Option<String>,
        aspect: Option<String>,
    ) -> Result<BoardNode> {
        self.update_node(board_id, node_id, |node| {
            if let Some(prompt) = prompt {
                let prompt = prompt.trim();
                if !prompt.is_empty() {
                    node.prompt = prompt.to_owned();
                }
            }
            if let Some(aspect) = aspect {
                node.aspect = aspect;
            }
            node.images.clear();
            node.image_labels.clear();
            node.attempts.clear();
            node.text.clear();
            node.status = NodeStatus::Running;
            node.error = None;
            node.stop_reason = None;
            node.run_started_at = Some(now_ms());
            node.finished_at = None;
            node.usage = None;
        })
    }

    pub fn move_nodes(&self, board_id: &str, positions: &[(String, f32, f32)]) -> Result<()> {
        if positions
            .iter()
            .any(|(_, x, y)| !x.is_finite() || !y.is_finite())
        {
            bail!("invalid node position");
        }
        let mut state = self.inner.write();
        let board = board_mut(&mut state, board_id)?;
        let positions_by_id: HashMap<&str, (f32, f32)> = positions
            .iter()
            .map(|(id, x, y)| (id.as_str(), (*x, *y)))
            .collect();
        for node in &mut board.nodes {
            if let Some((x, y)) = positions_by_id.get(node.id.as_str()) {
                node.x = Some(*x);
                node.y = Some(*y);
            }
        }
        drop(state);
        self.persist_and_notify();
        Ok(())
    }

    pub fn update_node(
        &self,
        board_id: &str,
        node_id: &str,
        update: impl FnOnce(&mut BoardNode),
    ) -> Result<BoardNode> {
        self.mutate_node(board_id, node_id, |node| {
            update(node);
            node.clone()
        })
    }

    /// Mutates a node without cloning it for a return value. Generation event
    /// handling uses this for frequent status, usage, and artifact updates.
    pub(crate) fn mutate_node<T>(
        &self,
        board_id: &str,
        node_id: &str,
        update: impl FnOnce(&mut BoardNode) -> T,
    ) -> Result<T> {
        let mut state = self.inner.write();
        let node = board_mut(&mut state, board_id)?
            .nodes
            .iter_mut()
            .find(|node| node.id == node_id)
            .context("Node not found")?;
        let result = update(node);
        drop(state);
        self.persist_and_notify();
        Ok(result)
    }

    pub fn delete_subtree(&self, board_id: &str, node_id: &str) -> Result<(Vec<String>, String)> {
        let mut state = self.inner.write();
        purge_expired_trash(&mut state);
        let board = board_mut(&mut state, board_id)?;
        if !board.nodes.iter().any(|node| node.id == node_id) {
            bail!("Node not found");
        }
        let mut children_by_parent: HashMap<&str, Vec<&str>> = HashMap::new();
        for node in &board.nodes {
            if let Some(parent_id) = node.parent_id.as_deref() {
                children_by_parent
                    .entry(parent_id)
                    .or_default()
                    .push(&node.id);
            }
        }
        let mut ids = HashSet::new();
        let mut pending = vec![node_id];
        while let Some(id) = pending.pop() {
            if !ids.insert(id.to_owned()) {
                continue;
            }
            if let Some(children) = children_by_parent.get(id) {
                pending.extend(children.iter().copied());
            }
        }
        drop(children_by_parent);
        let mut deleted = Vec::new();
        board.nodes.retain(|node| {
            if ids.contains(&node.id) {
                deleted.push(node.clone());
                false
            } else {
                true
            }
        });
        let deleted_ids = deleted.iter().map(|node| node.id.clone()).collect();
        let undo_id = Uuid::new_v4().to_string();
        state.trash.insert(
            undo_id.clone(),
            TrashEntry {
                board_id: board_id.to_owned(),
                nodes: deleted,
                expires_at: Instant::now() + TRASH_TTL,
            },
        );
        drop(state);
        self.persist_and_notify();
        Ok((deleted_ids, undo_id))
    }

    pub fn undo_delete(&self, board_id: &str, undo_id: &str) -> Result<Vec<String>> {
        let mut state = self.inner.write();
        purge_expired_trash(&mut state);
        let entry = state.trash.remove(undo_id).context("Nothing to undo")?;
        if entry.board_id != board_id {
            bail!("Nothing to undo");
        }
        let board = board_mut(&mut state, board_id)?;
        let existing: HashSet<_> = board.nodes.iter().map(|node| node.id.as_str()).collect();
        let restored: Vec<_> = entry
            .nodes
            .into_iter()
            .filter(|node| !existing.contains(node.id.as_str()))
            .collect();
        drop(existing);
        let ids = restored.iter().map(|node| node.id.clone()).collect();
        board.nodes.extend(restored);
        drop(state);
        self.persist_and_notify();
        Ok(ids)
    }

    pub fn mark_stopped(&self, board_id: &str, node_id: &str, reason: StopReason) -> Result<()> {
        self.update_node(board_id, node_id, |node| {
            if node.status == NodeStatus::Running {
                node.status = NodeStatus::Stopped;
                node.stop_reason = Some(reason);
                node.finished_at = Some(now_ms());
            }
        })?;
        Ok(())
    }

    pub fn image_path(&self, board_id: &str, url: &str) -> Option<PathBuf> {
        let name = url
            .strip_prefix("/images/")?
            .strip_prefix(board_id)?
            .strip_prefix('/')?;
        if Path::new(name).components().count() != 1 {
            return None;
        }
        Some(self.paths.images.join(board_id).join(name))
    }

    pub fn thumbnail_path(&self, board_id: &str, url: &str) -> Option<PathBuf> {
        let path = self.image_path(board_id, url)?;
        thumbnail_path_for(&path)
    }

    pub fn sprite_thumbnail_path(&self, board_id: &str, url: &str) -> Option<PathBuf> {
        let path = self.image_path(board_id, url)?;
        sprite_thumbnail_path_for(&path)
    }

    /// Conditions generated images saved by older app versions. Raw pixels are
    /// archived first, and a per-image marker makes the sweep a one-time cost.
    pub fn condition_existing_generated_images(&self) {
        if !crate::generation::conditioner::enabled() {
            return;
        }
        let candidates = {
            let state = self.inner.read();
            let mut seen = HashSet::new();
            let mut candidates = Vec::new();
            for board in &state.boards {
                for url in board.nodes.iter().flat_map(|node| &node.attempts) {
                    if seen.insert((board.id.as_str(), url.as_str())) {
                        candidates.push((board.id.clone(), url.clone()));
                    }
                }
            }
            candidates
        };

        let mut rewritten = false;
        for (board_id, url) in candidates {
            rewritten |= self
                .condition_existing_generated_image(&board_id, &url)
                .unwrap_or(false);
        }
        if rewritten {
            let _ = self.events.try_send(RepositoryEvent::ImagesRewritten);
        }
    }

    fn condition_existing_generated_image(&self, board_id: &str, url: &str) -> Result<bool> {
        let source = self
            .image_path(board_id, url)
            .filter(|path| path.is_file())
            .context("stored generated image was missing")?;
        let (original, marker) = self.generated_original_paths(board_id, &source)?;
        if marker.exists() {
            return Ok(false);
        }

        let recovering_interrupted_migration = original.exists();
        if !recovering_interrupted_migration {
            atomic_copy(&source, &original)?;
        }
        let temporary = source.with_file_name(format!(
            ".conditioned-{}-{}",
            Uuid::new_v4(),
            source.file_name().unwrap_or_default().to_string_lossy()
        ));
        let result = (|| -> Result<bool> {
            let applied =
                crate::generation::conditioner::condition_generated_image(&source, &temporary)?;
            if applied {
                crate::platform::replace_file(&temporary, &source)?;
            }
            if applied || recovering_interrupted_migration {
                remove_thumbnails(&source);
                create_thumbnail(&source)?;
            }
            atomic_write(&marker, b"conditioned-v1\n")?;
            Ok(applied || recovering_interrupted_migration)
        })();
        let _ = fs::remove_file(&temporary);
        result.with_context(|| format!("failed to condition {}", source.display()))
    }

    /// Re-creates every stored thumbnail that is smaller than the current
    /// `THUMBNAIL_MAX_DIMENSION` allows, then announces a change so open
    /// views pick the sharper files up. A one-time sweep after upgrades that
    /// raise the thumbnail size; run it on a background thread, it decodes
    /// every affected original.
    pub fn refresh_undersized_thumbnails(&self) {
        // Imports always create both current thumbnail sizes. This versioned
        // marker turns the upgrade scan into the one-time pass its contract
        // promises, while a dimension or algorithm change gets a new marker.
        let marker = self.paths.images.join(format!(
            ".thumbnail-sweep-v{THUMBNAIL_SWEEP_VERSION}-{THUMBNAIL_MAX_DIMENSION}-{SPRITE_THUMBNAIL_MAX_DIMENSION}"
        ));
        if marker.is_file() {
            return;
        }
        let Ok(boards) = fs::read_dir(&self.paths.images) else {
            return;
        };
        let mut regenerated = false;
        let mut complete = true;
        for board in boards {
            let Ok(board) = board else {
                complete = false;
                continue;
            };
            let board_path = board.path();
            if !board_path.is_dir() {
                continue;
            }
            let Ok(entries) = fs::read_dir(board_path) else {
                complete = false;
                continue;
            };
            for entry in entries {
                let Ok(entry) = entry else {
                    complete = false;
                    continue;
                };
                let path = entry.path();
                let is_thumbnail = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_none_or(|name| name.starts_with("t_") || name.starts_with("s_"));
                if is_thumbnail || !path.is_file() {
                    continue;
                }
                let (Some(thumbnail), Some(sprite)) =
                    (thumbnail_path_for(&path), sprite_thumbnail_path_for(&path))
                else {
                    continue;
                };
                let Ok(source_dimensions) = image::image_dimensions(&path) else {
                    complete = false;
                    continue;
                };
                // A missing or unreadable thumbnail beside a readable original
                // counts as undersized and is rebuilt.
                let undersized = |candidate: &Path, target: u32| {
                    image::image_dimensions(candidate).map_or(true, |(thumb_w, thumb_h)| {
                        thumb_w.max(thumb_h)
                            < source_dimensions.0.max(source_dimensions.1).min(target)
                    })
                };
                if undersized(&thumbnail, THUMBNAIL_MAX_DIMENSION) {
                    if create_thumbnail(&path).is_ok() {
                        regenerated = true;
                    } else {
                        complete = false;
                    }
                } else if undersized(&sprite, SPRITE_THUMBNAIL_MAX_DIMENSION) {
                    if create_sprite_thumbnail(&path).is_ok() {
                        regenerated = true;
                    } else {
                        complete = false;
                    }
                }
            }
        }
        if complete {
            let _ = atomic_write(&marker, b"complete\n");
        }
        if regenerated {
            // Thumbnail paths held by the view must be resolved again after a
            // sweep replaces files in place.
            let _ = self.events.try_send(RepositoryEvent::ImagesRewritten);
        }
    }

    pub fn emit_activity(&self, node_id: impl Into<String>, text: impl Into<String>) {
        let _ = self.events.try_send(RepositoryEvent::Activity {
            node_id: node_id.into(),
            text: text.into(),
        });
    }

    fn import_attachments(&self, board_id: &str, paths: &[PathBuf]) -> Result<Vec<String>> {
        let mut total = 0_u64;
        let mut prepared = Vec::new();
        for path in paths {
            let metadata = fs::metadata(path)
                .with_context(|| format!("could not read attachment {}", path.display()))?;
            if !metadata.is_file() {
                bail!("{} is not a file", path.display());
            }
            if metadata.len() > MAX_ATTACHMENT_BYTES {
                bail!(
                    "Attachment exceeds {} MB",
                    MAX_ATTACHMENT_BYTES / 1024 / 1024
                );
            }
            total += metadata.len();
            if total > MAX_ATTACHMENT_TOTAL_BYTES {
                bail!(
                    "Attachments exceed {} MB total",
                    MAX_ATTACHMENT_TOTAL_BYTES / 1024 / 1024
                );
            }
            image::ImageFormat::from_path(path)
                .with_context(|| format!("{} is not a supported image", path.display()))?;
            prepared.push(path);
        }
        let mut imported = Vec::new();
        let mut created = Vec::new();
        for source in prepared {
            match self.copy_into_board(board_id, source) {
                Ok((destination, url)) => {
                    created.push(destination);
                    imported.push(url);
                }
                Err(error) => {
                    for path in created {
                        remove_image_and_thumbnail(&path);
                    }
                    return Err(error);
                }
            }
        }
        Ok(imported)
    }

    pub fn import_generated(&self, board_id: &str, source: &Path) -> Result<String> {
        let metadata = fs::metadata(source)
            .with_context(|| format!("generated image no longer exists: {}", source.display()))?;
        if !metadata.is_file() || metadata.len() == 0 {
            bail!("generated image was empty");
        }
        let directory = self.paths.images.join(board_id);
        fs::create_dir_all(&directory)?;
        let name = fresh_image_name(source);
        let destination = directory.join(&name);
        let (original, marker) = self.generated_original_paths(board_id, &destination)?;
        let result = (|| -> Result<String> {
            atomic_copy(source, &original)?;
            let applied =
                crate::generation::conditioner::condition_generated_image(source, &destination)?;
            if !applied {
                fs::copy(source, &destination)?;
            }
            create_thumbnail(&destination)?;
            if crate::generation::conditioner::enabled() {
                atomic_write(&marker, b"conditioned-v1\n")?;
            }
            Ok(format!("/images/{board_id}/{name}"))
        })();
        if result.is_err() {
            remove_image_and_thumbnail(&destination);
            let _ = fs::remove_file(&original);
            let _ = fs::remove_file(&marker);
        }
        result
    }

    fn generated_original_paths(
        &self,
        board_id: &str,
        stored_image: &Path,
    ) -> Result<(PathBuf, PathBuf)> {
        let name = stored_image.file_name().context("image had no file name")?;
        let directory = self.paths.generated_originals.join(board_id);
        let original = directory.join(name);
        let marker = directory.join(format!("{}.conditioned-v1", name.to_string_lossy()));
        Ok((original, marker))
    }

    /// Copies `source` into the board's image directory under a fresh name and
    /// builds its thumbnail, returning the stored path and its `/images/…` URL.
    fn copy_into_board(&self, board_id: &str, source: &Path) -> Result<(PathBuf, String)> {
        let directory = self.paths.images.join(board_id);
        fs::create_dir_all(&directory)?;
        let name = fresh_image_name(source);
        let destination = directory.join(&name);
        fs::copy(source, &destination)?;
        if let Err(error) = create_thumbnail(&destination) {
            remove_image_and_thumbnail(&destination);
            return Err(error);
        }
        Ok((destination, format!("/images/{board_id}/{name}")))
    }

    /// Queues the mutated state for saving and tells open views to re-read it.
    /// Saving is deliberately not awaited: it happens on the writer thread so
    /// no caller — least of all the render thread — blocks on `fsync`.
    fn persist_and_notify(&self) {
        self.writer.request();
        let _ = self.events.try_send(RepositoryEvent::Changed);
    }

    /// Blocks until every mutation so far is on disk. Call before quitting.
    pub fn flush(&self) {
        self.writer.flush();
    }
}

fn write_boards(path: &Path, boards: &[Board]) -> Result<()> {
    atomic_write(path, &serde_json::to_vec_pretty(boards)?)
}

fn load_boards(path: &Path) -> Result<Vec<Board>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path)?;
    match serde_json::from_slice(&bytes) {
        Ok(boards) => Ok(boards),
        Err(error) => {
            let backup = path.with_extension(format!("json.corrupt-{}", now_ms()));
            fs::copy(path, &backup).with_context(|| {
                format!(
                    "boards were corrupt and could not be backed up to {}",
                    backup.display()
                )
            })?;
            Err(error).context(format!(
                "boards.json is unreadable; the original was preserved at {}",
                backup.display()
            ))
        }
    }
}

fn board_mut<'a>(state: &'a mut RepositoryState, board_id: &str) -> Result<&'a mut Board> {
    let board = state
        .boards
        .iter_mut()
        .find(|board| board.id == board_id)
        .context("Board not found")?;
    Ok(Arc::make_mut(board))
}

fn purge_expired_trash(state: &mut RepositoryState) {
    let now = Instant::now();
    state.trash.retain(|_, entry| entry.expires_at > now);
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("file has no parent directory")?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        crate::platform::replace_file(&temporary, path)?;
        crate::platform::sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(|| format!("failed to save {}", path.display()))
}

fn atomic_copy(source: &Path, destination: &Path) -> Result<()> {
    let parent = destination
        .parent()
        .context("destination has no parent directory")?;
    fs::create_dir_all(parent)?;
    let temporary = destination.with_extension(format!("tmp-{}", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        fs::copy(source, &temporary)?;
        File::open(&temporary)?.sync_all()?;
        crate::platform::replace_file(&temporary, destination)?;
        crate::platform::sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(|| {
        format!(
            "failed to preserve generated original {}",
            destination.display()
        )
    })
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<()> {
    if fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    atomic_write(path, bytes)
}

/// Builds both display thumbnails beside `source`: `t_` at
/// `THUMBNAIL_MAX_DIMENSION` and `s_` at `SPRITE_THUMBNAIL_MAX_DIMENSION`.
pub fn create_thumbnail(source: &Path) -> Result<()> {
    let image = image::ImageReader::open(source)?
        .with_guessed_format()?
        .decode()
        .with_context(|| format!("failed to decode {}", source.display()))?;
    let thumbnail = image.thumbnail(THUMBNAIL_MAX_DIMENSION, THUMBNAIL_MAX_DIMENSION);
    save_thumbnail(&thumbnail, source, thumbnail_path_for(source))?;
    let sprite = thumbnail.thumbnail(
        SPRITE_THUMBNAIL_MAX_DIMENSION,
        SPRITE_THUMBNAIL_MAX_DIMENSION,
    );
    save_thumbnail(&sprite, source, sprite_thumbnail_path_for(source))
}

/// Builds only the `s_` sprite thumbnail, downscaling the existing `t_` file
/// so a sweep never has to decode the full original twice.
fn create_sprite_thumbnail(source: &Path) -> Result<()> {
    let thumbnail = thumbnail_path_for(source).context("image had no file name")?;
    let image = image::ImageReader::open(&thumbnail)?
        .with_guessed_format()?
        .decode()
        .with_context(|| format!("failed to decode {}", thumbnail.display()))?;
    let sprite = image.thumbnail(
        SPRITE_THUMBNAIL_MAX_DIMENSION,
        SPRITE_THUMBNAIL_MAX_DIMENSION,
    );
    save_thumbnail(&sprite, source, sprite_thumbnail_path_for(source))
}

fn save_thumbnail(
    image: &image::DynamicImage,
    source: &Path,
    destination: Option<PathBuf>,
) -> Result<()> {
    let destination = destination.context("image had no file name")?;
    let result = if is_svg_embeddable_raster(source) {
        image.save(&destination)
    } else {
        image.save_with_format(&destination, image::ImageFormat::Png)
    };
    result.with_context(|| format!("failed to create thumbnail {}", destination.display()))
}

fn remove_image_and_thumbnail(source: &Path) {
    let _ = fs::remove_file(source);
    remove_thumbnails(source);
}

fn remove_thumbnails(source: &Path) {
    if let Some(name) = source.file_name() {
        for prefix in ["t_", "s_"] {
            let name = name.to_string_lossy();
            let _ = fs::remove_file(source.with_file_name(format!("{prefix}{name}")));
            let _ = fs::remove_file(source.with_file_name(format!("{prefix}{name}.png")));
        }
    }
}

fn fresh_image_name(source: &Path) -> String {
    format!(
        "{}-{}-{}",
        now_ms(),
        &Uuid::new_v4().to_string()[..8],
        sanitize_filename(&source.file_name().unwrap_or_default().to_string_lossy())
    )
}

pub fn thumbnail_path_for(source: &Path) -> Option<PathBuf> {
    prefixed_thumbnail_path(source, "t_")
}

pub fn sprite_thumbnail_path_for(source: &Path) -> Option<PathBuf> {
    prefixed_thumbnail_path(source, "s_")
}

fn prefixed_thumbnail_path(source: &Path, prefix: &str) -> Option<PathBuf> {
    let name = source.file_name()?.to_string_lossy();
    let suffix = if is_svg_embeddable_raster(source) {
        String::new()
    } else {
        ".png".to_owned()
    };
    Some(source.with_file_name(format!("{prefix}{name}{suffix}")))
}

fn is_svg_embeddable_raster(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "gif" | "jpeg" | "jpg" | "png" | "webp"
            )
        })
}

fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "image.png".into()
    } else {
        cleaned
    }
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::{
        DataPaths, Repository, RepositoryEvent, atomic_write, board_mut, create_thumbnail,
        thumbnail_path_for,
    };
    use crate::model::{BoardNode, NewNodesRequest, NodeStatus};
    use async_channel::{Receiver, unbounded};
    use image::{ColorType, DynamicImage, ImageFormat, Rgba, RgbaImage};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn repository(directory: &TempDir) -> (Repository, Receiver<RepositoryEvent>) {
        let generated = directory.path().join("generated");
        fs::create_dir_all(&generated).unwrap();
        let (sender, receiver) = unbounded();
        (
            Repository::open_at(
                DataPaths::at(directory.path().join("data"), generated),
                sender,
            )
            .unwrap(),
            receiver,
        )
    }

    fn save_patterned_image(path: &Path) {
        let image = RgbaImage::from_fn(64, 64, |x, y| {
            let offset = match (y % 2, x % 2) {
                (0, 0) => 1,
                (1, 1) => -1,
                _ => 0,
            };
            Rgba([
                (128_i16 + offset) as u8,
                (96_i16 + offset) as u8,
                (64_i16 + offset) as u8,
                255,
            ])
        });
        DynamicImage::ImageRgba8(image).save(path).unwrap();
    }

    fn node(id: impl Into<String>, parent_id: Option<String>) -> BoardNode {
        BoardNode {
            id: id.into(),
            parent_id,
            prompt: "test".into(),
            aspect: "auto".into(),
            source_images: Vec::new(),
            attachments: Vec::new(),
            images: Vec::new(),
            image_labels: Vec::new(),
            attempts: Vec::new(),
            text: String::new(),
            status: NodeStatus::Done,
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

    #[test]
    fn thumbnail_paths_preserve_svg_embeddable_formats() {
        assert_eq!(
            thumbnail_path_for(Path::new("/tmp/example.JPG")).as_deref(),
            Some(Path::new("/tmp/t_example.JPG"))
        );
        assert_eq!(
            thumbnail_path_for(Path::new("/tmp/example.webp")).as_deref(),
            Some(Path::new("/tmp/t_example.webp"))
        );
    }

    #[test]
    fn atomic_writes_replace_an_existing_file() {
        let directory = TempDir::new().unwrap();
        let destination = directory.path().join("boards.json");

        atomic_write(&destination, b"first").unwrap();
        atomic_write(&destination, b"second").unwrap();

        assert_eq!(fs::read(destination).unwrap(), b"second");
    }

    #[test]
    fn unsupported_sprite_formats_receive_png_thumbnails() {
        let directory = TempDir::new().expect("temporary directory");
        let source = directory.path().join("source.bmp");
        RgbaImage::from_pixel(8, 4, Rgba([23, 45, 67, 255]))
            .save_with_format(&source, ImageFormat::Bmp)
            .expect("BMP fixture");

        create_thumbnail(&source).expect("thumbnail");

        let thumbnail = directory.path().join("t_source.bmp.png");
        assert!(thumbnail.exists());
        assert_eq!(
            image::ImageReader::open(&thumbnail)
                .expect("open thumbnail")
                .with_guessed_format()
                .expect("detect thumbnail format")
                .format(),
            Some(ImageFormat::Png)
        );
    }

    #[test]
    fn deleting_a_deep_subtree_visits_each_node_once() {
        let directory = TempDir::new().unwrap();
        let (repository, _) = repository(&directory);
        let board = repository.create_board().unwrap();
        {
            let mut state = repository.inner.write();
            let board = board_mut(&mut state, &board.id).unwrap();
            board.nodes = (0..1_000)
                .map(|index| {
                    node(
                        format!("node-{index}"),
                        (index > 0).then(|| format!("node-{}", index - 1)),
                    )
                })
                .collect();
        }

        let (deleted, _) = repository.delete_subtree(&board.id, "node-0").unwrap();

        assert_eq!(deleted.len(), 1_000);
        assert!(repository.board(&board.id).unwrap().nodes.is_empty());
    }

    /// Board mutations arrive from the main thread and from every generation
    /// worker at once, and they no longer write the file themselves. Closing
    /// the repository must still leave every one of them on disk.
    #[test]
    fn concurrent_mutations_all_survive_a_reopen() {
        let directory = TempDir::new().unwrap();
        let (open, _receiver) = repository(&directory);
        let boards: Vec<_> = (0..8).map(|_| open.create_board().unwrap().id).collect();

        std::thread::scope(|scope| {
            for (index, board_id) in boards.iter().enumerate() {
                let open = open.clone();
                scope.spawn(move || {
                    for revision in 0..20 {
                        open.rename_board(board_id, &format!("board {index} rev {revision}"))
                            .unwrap();
                    }
                });
            }
        });
        drop(open);

        let (reopened, _) = repository(&directory);
        for (index, board_id) in boards.iter().enumerate() {
            assert_eq!(
                reopened.board(board_id).unwrap().title,
                format!("board {index} rev 19")
            );
        }
    }

    #[test]
    fn generated_import_archives_raw_bytes_and_stores_the_conditioned_copy() {
        let directory = TempDir::new().unwrap();
        let (repository, _) = repository(&directory);
        let board = repository.create_board().unwrap();
        let source = directory.path().join("generated/source.png");
        save_patterned_image(&source);
        let raw_bytes = fs::read(&source).unwrap();

        let url = repository.import_generated(&board.id, &source).unwrap();
        let stored = repository.image_path(&board.id, &url).unwrap();
        let (original, marker) = repository
            .generated_original_paths(&board.id, &stored)
            .unwrap();

        assert_eq!(fs::read(original).unwrap(), raw_bytes);
        assert!(marker.exists());
        assert_eq!(image::open(stored).unwrap().color(), ColorType::Rgba16);
    }

    #[test]
    fn legacy_generated_images_are_conditioned_once_and_emit_a_cache_refresh() {
        let directory = TempDir::new().unwrap();
        let (repository, receiver) = repository(&directory);
        let board = repository.create_board().unwrap();
        let source = directory.path().join("legacy.png");
        save_patterned_image(&source);
        let raw_bytes = fs::read(&source).unwrap();
        let (_, url) = repository.copy_into_board(&board.id, &source).unwrap();
        let node = repository
            .add_nodes(
                &board.id,
                NewNodesRequest {
                    prompt: "legacy".into(),
                    parent_id: None,
                    source_images: None,
                    aspect: "auto".into(),
                    count: 1,
                    attachment_paths: Vec::new(),
                    attachment_urls: Vec::new(),
                    position: None,
                },
            )
            .unwrap()
            .remove(0);
        repository
            .update_node(&board.id, &node.id, |node| {
                node.attempts = vec![url.clone()];
                node.images = vec![url.clone()];
            })
            .unwrap();
        while receiver.try_recv().is_ok() {}

        repository.condition_existing_generated_images();

        let stored = repository.image_path(&board.id, &url).unwrap();
        let (original, marker) = repository
            .generated_original_paths(&board.id, &stored)
            .unwrap();
        assert_eq!(fs::read(original).unwrap(), raw_bytes);
        assert!(marker.exists());
        assert_eq!(image::open(stored).unwrap().color(), ColorType::Rgba16);
        assert!(matches!(
            receiver.try_recv(),
            Ok(RepositoryEvent::ImagesRewritten)
        ));

        repository.condition_existing_generated_images();
        assert!(receiver.try_recv().is_err());
    }
}
