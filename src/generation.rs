use crate::manifest::{OutputManifest, absolute_file_path, is_path_inside};
use crate::model::{
    Board, BoardNode, MAX_ACTIVE_PER_BOARD, NewNodesRequest, NodeStatus, StopReason,
};
use crate::storage::{Repository, now_ms};
use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(1_200);
const FINAL_SWEEP_DELAY: Duration = Duration::from_millis(800);
const IDLE_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(4);
const LOGIN_SHELL_PATH_MARKER: &[u8] = b"__CODEXIMAGE_PATH__=";

const PREAMBLE: &str = r#"You are an expert image-generation assistant.

Hard rules:
- ALWAYS create every final visual deliverable implied by the request with your built-in image generation tool. Never draw images with code (SVG/HTML/canvas), never substitute placeholders, and never fetch images from the web.
- Infer the number of final deliverables from the request. A single scene normally needs one; a ten-page comic needs ten separate ordered images. Never combine multiple requested deliverables into a contact sheet or collage unless the user explicitly asks for that format.
- You may call the image generation tool again whenever an output needs correction. At the end, select the best final result for each intended deliverable and omit every superseded attempt.
- The app captures generated files automatically. Do NOT run shell commands to copy, move, inspect, or verify image files unless the user explicitly asks for file operations.
- Your final response must follow the supplied JSON schema. Put only selected final images in `outputs`, in the semantic order requested by the user. For each output, use the exact absolute saved path returned by the image generation tool and a short identifying label. Never include a superseded attempt. Set `complete` to true only when the selected outputs fulfill the entire request; otherwise set it to false. Keep `summary` to one concise sentence.
- Structured progress updates are not final selections: while any render is pending, set `complete` to false and leave `outputs` empty. Populate `outputs` only in the terminal response after every render and correction has settled.

Prompting the image tool:
- Rewrite the request into a clean spec ordered scene/backdrop -> subject -> key details -> constraints, and include the intended use to set the polish level. For complex requests use short labeled lines.
- Match augmentation to specificity. Never invent characters, props, brands, slogans, palettes, or story beats the user did not imply.
- For photorealism, use photography language and ask for real-world texture and imperfect everyday detail.
- If text must appear in the image, quote it verbatim, specify typography and placement, spell uncommon words letter-by-letter, and require exact rendering with no extra characters.
- When image files are provided, treat each by its stated role. For compositing, match lighting, perspective, and scale.
- For edits, state invariants explicitly. Preserve identity aggressively when people are involved and preserve everything the request does not ask to change."#;

#[derive(Clone)]
pub struct GenerationEngine {
    inner: Arc<EngineInner>,
}

struct EngineInner {
    repository: Repository,
    jobs: Mutex<HashMap<String, Arc<JobControl>>>,
    codex: OnceLock<CodexInvocation>,
}

struct CodexInvocation {
    executable: PathBuf,
    path: OsString,
}

struct JobControl {
    board_id: String,
    node_id: String,
    pid: AtomicI32,
    termination: Mutex<Option<Termination>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Termination {
    User,
    AppQuit,
    Deleted,
    Timeout,
    Replaced,
}

impl Termination {
    /// The reason recorded on the node, or `None` when the termination is not
    /// user-visible as a stop (a timeout becomes an error, a replaced job is
    /// finalized by the run that replaced it).
    fn stop_reason(self) -> Option<StopReason> {
        match self {
            Self::User => Some(StopReason::User),
            Self::AppQuit => Some(StopReason::AppQuit),
            Self::Deleted => Some(StopReason::Deleted),
            Self::Timeout | Self::Replaced => None,
        }
    }
}

#[derive(Clone, Copy)]
enum Sandbox {
    WorkspaceWrite,
    ReadOnly,
}

impl Sandbox {
    fn as_arg(self) -> &'static str {
        match self {
            Self::WorkspaceWrite => "workspace-write",
            Self::ReadOnly => "read-only",
        }
    }
}

struct Runtime {
    watchers: HashSet<String>,
    sizes: HashMap<PathBuf, u64>,
    seen: HashSet<PathBuf>,
    artifacts: HashMap<PathBuf, String>,
    last_agent_message: Option<String>,
    turn_completed: bool,
    failures: Vec<String>,
    last_activity: Instant,
}

impl Runtime {
    fn new() -> Self {
        Self {
            watchers: HashSet::new(),
            sizes: HashMap::new(),
            seen: HashSet::new(),
            artifacts: HashMap::new(),
            last_agent_message: None,
            turn_completed: false,
            failures: Vec::new(),
            last_activity: Instant::now(),
        }
    }
}

impl GenerationEngine {
    pub fn new(repository: Repository) -> Self {
        Self {
            inner: Arc::new(EngineInner {
                repository,
                jobs: Mutex::new(HashMap::new()),
                codex: OnceLock::new(),
            }),
        }
    }

    fn codex_invocation(&self) -> &CodexInvocation {
        self.inner.codex.get_or_init(CodexInvocation::resolve)
    }

    pub fn repository(&self) -> Repository {
        self.inner.repository.clone()
    }

    pub fn active_count(&self) -> usize {
        self.inner.jobs.lock().len()
    }

    pub fn active_node_ids(&self) -> HashSet<String> {
        self.inner.jobs.lock().keys().cloned().collect()
    }

    pub fn add_and_start(
        &self,
        board_id: &str,
        request: NewNodesRequest,
    ) -> Result<Vec<BoardNode>> {
        let active_on_board = self
            .inner
            .jobs
            .lock()
            .values()
            .filter(|job| job.board_id == board_id)
            .count();
        let count = request.count.clamp(1, 4);
        if active_on_board + count > MAX_ACTIVE_PER_BOARD {
            bail!("Too many generations running on this board (max {MAX_ACTIVE_PER_BOARD})");
        }
        let nodes = self.inner.repository.add_nodes(board_id, request)?;
        let total = nodes.len();
        for (index, node) in nodes.iter().enumerate() {
            self.start_job(board_id, &node.id, index, total)?;
        }
        Ok(nodes)
    }

    pub fn regenerate(
        &self,
        board_id: &str,
        node_id: &str,
        prompt: Option<String>,
        aspect: Option<String>,
    ) -> Result<()> {
        self.stop(node_id, Termination::Replaced, libc::SIGKILL);
        self.inner
            .repository
            .regenerate_node(board_id, node_id, prompt, aspect)?;
        self.start_job(board_id, node_id, 0, 1)
    }

    pub fn stop_node(&self, node_id: &str) {
        self.stop(node_id, Termination::User, libc::SIGTERM);
    }

    pub fn delete_subtree(&self, board_id: &str, node_id: &str) -> Result<(Vec<String>, String)> {
        let (ids, undo_id) = self.inner.repository.delete_subtree(board_id, node_id)?;
        for id in &ids {
            self.stop(id, Termination::Deleted, libc::SIGKILL);
        }
        Ok((ids, undo_id))
    }

    pub fn delete_board(&self, board_id: &str) -> Result<()> {
        let ids: Vec<_> = self
            .inner
            .jobs
            .lock()
            .values()
            .filter(|job| job.board_id == board_id)
            .map(|job| job.node_id.clone())
            .collect();
        for id in ids {
            self.stop(&id, Termination::Deleted, libc::SIGKILL);
        }
        self.inner.repository.delete_board(board_id)
    }

    pub fn stop_all_for_quit(&self) {
        let ids: Vec<_> = self.inner.jobs.lock().keys().cloned().collect();
        for id in ids {
            self.stop(&id, Termination::AppQuit, libc::SIGKILL);
        }
        let _ = self.inner.repository.persist();
    }

    fn start_job(&self, board_id: &str, node_id: &str, index: usize, count: usize) -> Result<()> {
        let board = self
            .inner
            .repository
            .board(board_id)
            .context("Board not found")?;
        let node = board
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .cloned()
            .context("Node not found")?;
        let control = Arc::new(JobControl {
            board_id: board_id.to_owned(),
            node_id: node_id.to_owned(),
            pid: AtomicI32::new(0),
            termination: Mutex::new(None),
        });
        self.inner
            .jobs
            .lock()
            .insert(node_id.to_owned(), control.clone());
        let engine = self.clone();
        thread::Builder::new()
            .name(format!(
                "codex-generation-{}",
                &node_id[..node_id.len().min(8)]
            ))
            .spawn(move || engine.run_job(board, node, control, index, count))
            .context("failed to start generation worker")?;
        Ok(())
    }

    fn run_job(
        &self,
        board: Board,
        node: BoardNode,
        control: Arc<JobControl>,
        index: usize,
        count: usize,
    ) {
        if let Err(error) = self.run_job_inner(&board, &node, &control, index, count)
            && self.is_current(&control)
        {
            let message = error.to_string();
            let _ = self
                .inner
                .repository
                .update_node(&board.id, &node.id, |node| {
                    if node.status == NodeStatus::Running {
                        node.status = NodeStatus::Error;
                        node.error = Some(message);
                        node.finished_at = Some(now_ms());
                    }
                });
        }
        self.finish_control(&control);
    }

    fn run_job_inner(
        &self,
        board: &Board,
        node: &BoardNode,
        control: &Arc<JobControl>,
        index: usize,
        count: usize,
    ) -> Result<()> {
        if control.termination.lock().is_some() {
            return self.finalize(board, node, control, &mut Runtime::new());
        }
        let workspace = self.inner.repository.paths().workspaces.join(&board.id);
        fs::create_dir_all(&workspace)?;
        let prompt = build_node_prompt(&self.inner.repository, board, node, index, count);
        let mut child = self
            .codex_exec(Sandbox::WorkspaceWrite, &workspace, prompt)
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to launch {}",
                    self.codex_invocation().executable.display()
                )
            })?;
        control.pid.store(child.id() as i32, Ordering::Release);
        if control.termination.lock().is_some() {
            kill_process_group(child.id() as i32, libc::SIGKILL);
        }
        let stdout = child
            .stdout
            .take()
            .context("Codex stdout was unavailable")?;
        let stderr = child
            .stderr
            .take()
            .context("Codex stderr was unavailable")?;
        let (line_tx, line_rx) = mpsc::channel();
        let stdout_thread = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line_tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr_thread = thread::spawn(move || read_tail(stderr, 4 * 1024));
        let mut log = OpenOptions::new().create(true).append(true).open(
            self.inner
                .repository
                .paths()
                .logs
                .join(format!("{}.jsonl", board.id)),
        )?;
        let mut runtime = Runtime::new();
        let mut last_poll = Instant::now();
        let exit_status = loop {
            while let Ok(line) = line_rx.try_recv() {
                runtime.last_activity = Instant::now();
                writeln!(log, "{line}")?;
                self.handle_event(board, node, &mut runtime, &line);
            }
            if last_poll.elapsed() >= POLL_INTERVAL {
                if self.collect_images(board, node, control, &mut runtime)? {
                    runtime.last_activity = Instant::now();
                }
                last_poll = Instant::now();
            }
            if control.termination.lock().is_none()
                && runtime.last_activity.elapsed() >= IDLE_TIMEOUT
            {
                *control.termination.lock() = Some(Termination::Timeout);
                kill_process_group(control.pid.load(Ordering::Acquire), libc::SIGKILL);
            }
            if let Some(status) = child.try_wait()? {
                break status;
            }
            thread::sleep(Duration::from_millis(80));
        };
        let _ = stdout_thread.join();
        while let Ok(line) = line_rx.try_recv() {
            writeln!(log, "{line}")?;
            self.handle_event(board, node, &mut runtime, &line);
        }
        let stderr_tail = stderr_thread.join().unwrap_or_default();
        if !exit_status.success() && control.termination.lock().is_none() {
            runtime.failures.push(if stderr_tail.trim().is_empty() {
                format!("codex exited with {exit_status}")
            } else {
                tail_chars(stderr_tail.trim(), 1_000)
            });
        }

        let _ = self.collect_images(board, node, control, &mut runtime);
        thread::sleep(FINAL_SWEEP_DELAY);
        let _ = self.collect_images(board, node, control, &mut runtime);
        if !self.is_current(control) {
            return Ok(());
        }
        self.finalize(board, node, control, &mut runtime)
    }

    fn handle_event(&self, board: &Board, node: &BoardNode, runtime: &mut Runtime, line: &str) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("thread.started") => {
                if let Some(thread_id) = event.get("thread_id").and_then(Value::as_str) {
                    runtime.watchers.insert(thread_id.to_owned());
                }
            }
            Some("item.completed") => {
                let Some(item) = event.get("item") else {
                    return;
                };
                match item.get("type").and_then(Value::as_str) {
                    Some("agent_message") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            runtime.last_agent_message = Some(text.to_owned());
                        }
                    }
                    Some("reasoning") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            self.inner.repository.emit_activity(
                                &node.id,
                                tail_chars(text.lines().next().unwrap_or_default(), 140),
                            );
                        }
                    }
                    Some("command_execution") => {
                        if let Some(command) = item.get("command").and_then(Value::as_str) {
                            self.inner.repository.emit_activity(
                                &node.id,
                                format!("Running: {}", tail_chars(command, 140)),
                            );
                        }
                    }
                    _ => {}
                }
            }
            Some("turn.completed") => {
                runtime.turn_completed = true;
                if let Some(usage) = event.get("usage").and_then(Value::as_object) {
                    let additions: BTreeMap<String, u64> = usage
                        .iter()
                        .filter_map(|(key, value)| value.as_u64().map(|value| (key.clone(), value)))
                        .collect();
                    let _ = self
                        .inner
                        .repository
                        .update_node(&board.id, &node.id, |node| {
                            let usage = node.usage.get_or_insert_with(BTreeMap::new);
                            for (key, value) in additions {
                                *usage.entry(key).or_default() += value;
                            }
                        });
                }
            }
            Some("turn.failed") | Some("error") => {
                let message = event
                    .pointer("/error/message")
                    .or_else(|| event.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Generation failed");
                runtime.failures.push(message.to_owned());
            }
            _ => {}
        }
    }

    fn collect_images(
        &self,
        board: &Board,
        node: &BoardNode,
        control: &Arc<JobControl>,
        runtime: &mut Runtime,
    ) -> Result<bool> {
        if !self.is_current(control) {
            return Ok(false);
        }
        let mut changed = false;
        for thread_id in runtime.watchers.clone() {
            let directory = self
                .inner
                .repository
                .paths()
                .generated_images
                .join(&thread_id);
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if runtime.seen.contains(&path) {
                    continue;
                }
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if !metadata.is_file() || metadata.len() == 0 {
                    continue;
                }
                if image::ImageFormat::from_path(&path).is_err() {
                    continue;
                }
                if runtime.sizes.insert(path.clone(), metadata.len()) != Some(metadata.len()) {
                    continue;
                }
                let canonical = path.canonicalize()?;
                if !is_path_inside(&directory, &canonical) {
                    continue;
                }
                let url = self
                    .inner
                    .repository
                    .import_generated(&board.id, &canonical)?;
                runtime.seen.insert(path);
                runtime.artifacts.insert(canonical, url.clone());
                self.inner
                    .repository
                    .update_node(&board.id, &node.id, |node| {
                        if !node.attempts.contains(&url) {
                            node.attempts.push(url.clone());
                        }
                    })?;
                changed = true;
            }
        }
        Ok(changed)
    }

    fn finalize(
        &self,
        board: &Board,
        node: &BoardNode,
        control: &Arc<JobControl>,
        runtime: &mut Runtime,
    ) -> Result<()> {
        let termination = *control.termination.lock();
        if let Some(termination) = termination {
            if let Some(reason) = termination.stop_reason() {
                return self
                    .inner
                    .repository
                    .mark_stopped(&board.id, &node.id, reason);
            }
            // A replaced job is finalized by the run that replaced it.
            return match termination {
                Termination::Timeout => self.record_timeout(board, node),
                _ => Ok(()),
            };
        }

        let generation_failure = runtime.failures.first().cloned();
        let mut manifest = runtime
            .turn_completed
            .then_some(runtime.last_agent_message.as_deref())
            .flatten()
            .and_then(|message| crate::manifest::parse(message).ok())
            .filter(|manifest| !manifest.outputs.is_empty() || runtime.artifacts.is_empty());

        if manifest.is_none() && !runtime.artifacts.is_empty() {
            self.inner.repository.emit_activity(
                &node.id,
                format!("Finalizing {} generated images", runtime.artifacts.len()),
            );
            manifest = self
                .recover_selection(board, node, control, runtime, generation_failure.as_deref())
                .ok()
                .flatten();
        }

        let mut error = None;
        if let Some(manifest) = manifest {
            if let Err(apply_error) = self.apply_manifest(board, node, runtime, &manifest) {
                error = Some(format!("Automatic final selection failed: {apply_error}"));
            } else if !manifest.complete {
                error = Some(if manifest.summary.is_empty() {
                    "Only part of the requested output set was completed.".into()
                } else {
                    manifest.summary.clone()
                });
            }
        } else {
            error = Some(generation_failure.unwrap_or_else(|| {
                if runtime.artifacts.is_empty() {
                    "Generation failed before producing an image.".into()
                } else {
                    format!(
                        "Automatic final selection failed; {} generated image(s) remain unfinalized.",
                        runtime.artifacts.len()
                    )
                }
            }));
        }
        self.inner
            .repository
            .update_node(&board.id, &node.id, |node| {
                if node.status != NodeStatus::Running {
                    return;
                }
                node.status = if error.is_some() {
                    NodeStatus::Error
                } else {
                    NodeStatus::Done
                };
                node.error = error;
                node.finished_at = Some(now_ms());
            })?;
        Ok(())
    }

    fn record_timeout(&self, board: &Board, node: &BoardNode) -> Result<()> {
        let attempts = self
            .inner
            .repository
            .node(&board.id, &node.id)
            .map(|node| node.attempts.len())
            .unwrap_or(0);
        self.inner
            .repository
            .update_node(&board.id, &node.id, |node| {
                node.status = NodeStatus::Error;
                node.error = Some(format!(
                    "No generation activity for 20 minutes. {}",
                    if attempts == 0 {
                        "No images were generated.".to_owned()
                    } else {
                        format!("{attempts} unfinalized image(s) were saved before the timeout.")
                    }
                ));
                node.finished_at = Some(now_ms());
            })?;
        Ok(())
    }

    fn apply_manifest(
        &self,
        board: &Board,
        node: &BoardNode,
        runtime: &mut Runtime,
        manifest: &OutputManifest,
    ) -> Result<()> {
        let mut images = Vec::new();
        let mut labels = Vec::new();
        for output in &manifest.outputs {
            let canonical = absolute_file_path(&output.path)?.canonicalize()?;
            if !runtime.watchers.iter().any(|thread_id| {
                is_path_inside(
                    &self
                        .inner
                        .repository
                        .paths()
                        .generated_images
                        .join(thread_id),
                    &canonical,
                )
            }) {
                bail!("a selected image was not generated by this run");
            }
            let url = match runtime.artifacts.get(&canonical) {
                Some(url) => url.clone(),
                None => {
                    let url = self
                        .inner
                        .repository
                        .import_generated(&board.id, &canonical)?;
                    runtime.artifacts.insert(canonical, url.clone());
                    url
                }
            };
            images.push(url);
            labels.push(output.label.clone());
        }
        self.inner
            .repository
            .update_node(&board.id, &node.id, |node| {
                node.images = images;
                node.image_labels = labels;
                node.text = manifest.summary.clone();
            })?;
        Ok(())
    }

    fn recover_selection(
        &self,
        board: &Board,
        node: &BoardNode,
        control: &Arc<JobControl>,
        runtime: &Runtime,
        failure: Option<&str>,
    ) -> Result<Option<OutputManifest>> {
        if !self.is_current(control) {
            return Ok(None);
        }
        let workspace = self.inner.repository.paths().workspaces.join(&board.id);
        let prompt = selection_recovery_prompt(node, runtime.artifacts.keys(), failure);
        let child = self
            .codex_exec(Sandbox::ReadOnly, &workspace, prompt)
            .spawn()?;
        control.pid.store(child.id() as i32, Ordering::Release);
        let output = child.wait_with_output()?;
        if control.termination.lock().is_some() {
            return Ok(None);
        }
        if !output.status.success() {
            return Ok(None);
        }
        let mut last_message = None;
        let mut completed = false;
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if event.get("type").and_then(Value::as_str) == Some("turn.completed") {
                completed = true;
            }
            if event.get("type").and_then(Value::as_str) == Some("item.completed")
                && event.pointer("/item/type").and_then(Value::as_str) == Some("agent_message")
            {
                last_message = event
                    .pointer("/item/text")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
        }
        Ok(if completed {
            last_message.and_then(|message| crate::manifest::parse(&message).ok())
        } else {
            None
        })
    }

    /// Builds a `codex exec` invocation that streams JSON events for `prompt`
    /// from within `workspace`, in its own process group so the whole job tree
    /// can be signalled at once.
    fn codex_exec(&self, sandbox: Sandbox, workspace: &Path, prompt: String) -> Command {
        let mut command = self.codex_invocation().command();
        command
            .arg("exec")
            .arg("-s")
            .arg(sandbox.as_arg())
            .arg("-C")
            .arg(workspace)
            .arg("--json")
            .arg("--output-schema")
            .arg(&self.inner.repository.paths().output_schema)
            .arg("--skip-git-repo-check")
            .arg(prompt)
            .current_dir(workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_process_group(&mut command);
        command
    }

    fn is_current(&self, control: &Arc<JobControl>) -> bool {
        self.inner
            .jobs
            .lock()
            .get(&control.node_id)
            .is_some_and(|current| Arc::ptr_eq(current, control))
    }

    fn finish_control(&self, control: &Arc<JobControl>) {
        control.pid.store(0, Ordering::Release);
        let mut jobs = self.inner.jobs.lock();
        if jobs
            .get(&control.node_id)
            .is_some_and(|current| Arc::ptr_eq(current, control))
        {
            jobs.remove(&control.node_id);
        }
    }

    fn stop(&self, node_id: &str, reason: Termination, signal: i32) {
        let control = self.inner.jobs.lock().get(node_id).cloned();
        let Some(control) = control else { return };
        *control.termination.lock() = Some(reason);
        let pid = control.pid.load(Ordering::Acquire);
        kill_process_group(pid, signal);
        if signal == libc::SIGTERM {
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(3));
                if control.pid.load(Ordering::Acquire) != 0 {
                    kill_process_group(control.pid.load(Ordering::Acquire), libc::SIGKILL);
                }
            });
        }
    }
}

fn build_node_prompt(
    repository: &Repository,
    board: &Board,
    node: &BoardNode,
    index: usize,
    count: usize,
) -> String {
    let mut sections = vec![PREAMBLE.to_owned()];
    let by_id: HashMap<_, _> = board
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut ancestors = Vec::new();
    let mut current = node
        .parent_id
        .as_deref()
        .and_then(|id| by_id.get(id).copied());
    while let Some(ancestor) = current {
        let prompt = if ancestor.prompt.chars().count() > 400 {
            format!("{}…", ancestor.prompt.chars().take(397).collect::<String>())
        } else {
            ancestor.prompt.clone()
        };
        ancestors.push(prompt);
        if ancestors.len() == 12 {
            break;
        }
        current = ancestor
            .parent_id
            .as_deref()
            .and_then(|id| by_id.get(id).copied());
    }
    ancestors.reverse();
    if !ancestors.is_empty() {
        sections.push(format!(
            "This request continues earlier work on an image. The prompts so far, oldest first:\n{}",
            ancestors
                .iter()
                .enumerate()
                .map(|(index, prompt)| format!("{}. {prompt}", index + 1))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    let source_paths: Vec<_> = node
        .source_images
        .iter()
        .filter_map(|url| repository.image_path(&board.id, url))
        .filter(|path| path.exists())
        .collect();
    if !source_paths.is_empty() {
        sections.push(format!(
            "The current image to continue from is saved at:\n{}\nView it first. The request below applies to this image: keep everything it does not ask to change.",
            bullet_paths(&source_paths)
        ));
    }
    let attachments: Vec<_> = node
        .attachments
        .iter()
        .filter_map(|url| repository.image_path(&board.id, url))
        .filter(|path| path.exists())
        .collect();
    if !attachments.is_empty() {
        sections.push(format!(
            "The user attached reference image file(s). View them before generating:\n{}",
            bullet_paths(&attachments)
        ));
    }
    sections.push(format!("Request: {}", node.prompt));
    let mut extras = Vec::new();
    if node.aspect != "auto" {
        extras.push(format!("Aspect ratio: {}.", node.aspect));
    }
    if count > 1 {
        extras.push(format!(
            "{count} independent takes of this entire request are generated in parallel; this is take {}. Give this take its own distinct interpretation while still producing every final deliverable implied by the request.",
            index + 1
        ));
    }
    if !extras.is_empty() {
        sections.push(extras.join(" "));
    }
    sections.join("\n\n")
}

fn selection_recovery_prompt<'a>(
    node: &BoardNode,
    candidates: impl Iterator<Item = &'a PathBuf>,
    failure: Option<&str>,
) -> String {
    let mut lines = vec![
        "You are finalizing an interrupted image-generation run.".to_owned(),
        "Hard rules: Do not generate, edit, copy, move, or delete images. View only the candidates below. Select the strongest final candidate for each intended deliverable in semantic order. Omit superseded attempts and duplicates. Return the supplied JSON schema and set complete accurately.".to_owned(),
        format!("Original request: {}", node.prompt),
    ];
    if let Some(failure) = failure {
        lines.push(format!("Generation interruption: {failure}"));
    }
    lines.push("Candidate files:".into());
    lines.extend(
        candidates
            .enumerate()
            .map(|(index, path)| format!("{}. {}", index + 1, path.display())),
    );
    lines.join("\n\n")
}

fn bullet_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tail_chars(value: &str, limit: usize) -> String {
    let count = value.chars().count();
    value.chars().skip(count.saturating_sub(limit)).collect()
}

/// Drains `reader` and keeps at most the last `limit` bytes.
fn read_tail_bytes(mut reader: impl Read, limit: usize) -> Vec<u8> {
    let mut tail = Vec::with_capacity(limit);
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                tail.extend_from_slice(&chunk[..read]);
                if tail.len() > limit {
                    tail.drain(..tail.len() - limit);
                }
            }
        }
    }
    tail
}

fn read_tail(reader: impl Read, limit: usize) -> String {
    String::from_utf8_lossy(&read_tail_bytes(reader, limit)).into_owned()
}

impl CodexInvocation {
    fn resolve() -> Self {
        let path = build_command_path(
            login_shell_path(),
            env::var_os("PATH"),
            dirs::home_dir().as_deref(),
        );
        let executable = env::var_os("CODEX_BIN")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                find_executable_on_path(OsStr::new("codex"), &path)
                    .unwrap_or_else(|| PathBuf::from("codex"))
            });
        Self { executable, path }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command.env("PATH", &self.path);
        command
    }
}

fn build_command_path(
    login_shell: Option<OsString>,
    inherited: Option<OsString>,
    home: Option<&Path>,
) -> OsString {
    let mut entries = Vec::new();
    if let Some(path) = login_shell {
        entries.extend(env::split_paths(&path));
    }
    if let Some(path) = inherited.as_ref() {
        entries.extend(env::split_paths(path));
    }
    if let Some(home) = home {
        entries.extend([
            home.join(".bun/bin"),
            home.join(".local/bin"),
            home.join(".cargo/bin"),
            home.join(".volta/bin"),
            home.join(".npm-global/bin"),
            home.join("Library/pnpm"),
        ]);
        let nvm_versions = home.join(".nvm/versions/node");
        if let Ok(versions) = fs::read_dir(nvm_versions) {
            let mut version_bins: Vec<_> = versions
                .filter_map(Result::ok)
                .map(|entry| entry.path().join("bin"))
                .filter(|path| path.is_dir())
                .collect();
            version_bins.sort_by(|left, right| right.cmp(left));
            entries.extend(version_bins);
        }
    }
    entries.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/usr/sbin"),
        PathBuf::from("/sbin"),
    ]);

    let mut seen = HashSet::new();
    entries.retain(|entry| !entry.as_os_str().is_empty() && seen.insert(entry.clone()));
    env::join_paths(entries).unwrap_or_else(|_| {
        inherited.unwrap_or_else(|| OsString::from("/usr/bin:/bin:/usr/sbin:/sbin"))
    })
}

fn find_executable_on_path(name: &OsStr, path: &OsStr) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn login_shell_path() -> Option<OsString> {
    let shell = env::var_os("SHELL")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from("/bin/zsh"));
    let mut command = Command::new(shell);
    command
        .args(["-ilc", "printf '\n__CODEXIMAGE_PATH__=%s\n' \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    configure_process_group(&mut command);
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let output = thread::spawn(move || read_tail_bytes(stdout, 64 * 1024));
    let deadline = Instant::now() + LOGIN_SHELL_TIMEOUT;
    let succeeded = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                terminate_process(&mut child);
                break false;
            }
            Err(_) => {
                terminate_process(&mut child);
                break false;
            }
        }
    };
    let bytes = output.join().ok()?;
    succeeded.then(|| parse_login_shell_path(&bytes)).flatten()
}

fn terminate_process(child: &mut std::process::Child) {
    #[cfg(unix)]
    kill_process_group(child.id() as i32, libc::SIGKILL);
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn parse_login_shell_path(output: &[u8]) -> Option<OsString> {
    use std::os::unix::ffi::OsStringExt;
    let start = output
        .windows(LOGIN_SHELL_PATH_MARKER.len())
        .rposition(|window| window == LOGIN_SHELL_PATH_MARKER)?
        + LOGIN_SHELL_PATH_MARKER.len();
    let end = output[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(output.len(), |offset| start + offset);
    (end > start).then(|| OsString::from_vec(output[start..end].to_vec()))
}

#[cfg(not(unix))]
fn parse_login_shell_path(output: &[u8]) -> Option<OsString> {
    let output = String::from_utf8_lossy(output);
    output
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("__CODEXIMAGE_PATH__="))
        .filter(|path| !path.is_empty())
        .map(OsString::from)
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_: &mut Command) {}

#[cfg(unix)]
fn kill_process_group(pid: i32, signal: i32) {
    if pid > 0 {
        unsafe {
            libc::kill(-pid, signal);
        }
    }
}

#[cfg(not(unix))]
fn kill_process_group(_: i32, _: i32) {}

#[cfg(test)]
mod tests {
    use super::{
        GenerationEngine, JobControl, Termination, build_command_path, find_executable_on_path,
        parse_login_shell_path, read_tail,
    };
    use crate::model::{NewNodesRequest, NodeStatus, StopReason};
    use crate::storage::{DataPaths, Repository};
    use async_channel::unbounded;
    use parking_lot::Mutex;
    use std::ffi::{OsStr, OsString};
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicI32;
    use tempfile::TempDir;

    #[test]
    fn stderr_tail_drains_input_and_retains_the_requested_suffix() {
        let input: Vec<_> = (0_u32..20_000)
            .map(|value| b'a' + (value % 26) as u8)
            .collect();
        let result = read_tail(Cursor::new(&input), 4_096);
        assert_eq!(result.as_bytes(), &input[input.len() - 4_096..]);
    }

    #[test]
    fn command_path_prefers_the_login_shell_and_deduplicates_entries() {
        let directory = TempDir::new().unwrap();
        let login = OsString::from("/login/bin:/shared/bin");
        let inherited = OsString::from("/shared/bin:/inherited/bin");

        let path = build_command_path(Some(login), Some(inherited), Some(directory.path()));
        let entries: Vec<_> = std::env::split_paths(&path).collect();

        assert_eq!(entries[0], PathBuf::from("/login/bin"));
        assert_eq!(entries[1], PathBuf::from("/shared/bin"));
        assert_eq!(entries[2], PathBuf::from("/inherited/bin"));
        assert_eq!(
            entries
                .iter()
                .filter(|entry| *entry == &PathBuf::from("/shared/bin"))
                .count(),
            1
        );
        assert!(entries.contains(&directory.path().join(".bun/bin")));
        assert!(entries.contains(&PathBuf::from("/opt/homebrew/bin")));
    }

    #[cfg(unix)]
    #[test]
    fn executable_resolution_finds_an_executable_file() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TempDir::new().unwrap();
        let executable = directory.path().join("codex");
        std::fs::write(&executable, "#!/bin/sh\n").unwrap();
        let mut permissions = executable.metadata().unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).unwrap();
        let path = std::env::join_paths([directory.path()]).unwrap();

        assert_eq!(
            find_executable_on_path(OsStr::new("codex"), &path),
            Some(executable)
        );
    }

    #[cfg(unix)]
    #[test]
    fn login_shell_path_parser_ignores_shell_startup_output() {
        let output = b"startup noise\n__CODEXIMAGE_PATH__=/user/bin:/usr/bin\n";

        assert_eq!(
            parse_login_shell_path(output),
            Some(OsString::from("/user/bin:/usr/bin"))
        );
    }

    #[test]
    fn cancellation_before_spawn_finishes_the_node_without_launching_codex() {
        let directory = TempDir::new().unwrap();
        let generated = directory.path().join("generated");
        std::fs::create_dir_all(&generated).unwrap();
        let (sender, _receiver) = unbounded();
        let repository = Repository::open_at(
            DataPaths::at(directory.path().join("data"), generated),
            sender,
        )
        .unwrap();
        let board = repository.create_board().unwrap();
        let node = repository
            .add_nodes(
                &board.id,
                NewNodesRequest {
                    prompt: "test".into(),
                    parent_id: None,
                    source_images: None,
                    aspect: "auto".into(),
                    count: 1,
                    attachment_paths: Vec::new(),
                    attachment_urls: Vec::new(),
                },
            )
            .unwrap()
            .remove(0);
        let board = repository.board(&board.id).unwrap();
        let engine = GenerationEngine::new(repository.clone());
        let control = Arc::new(JobControl {
            board_id: board.id.clone(),
            node_id: node.id.clone(),
            pid: AtomicI32::new(0),
            termination: Mutex::new(Some(Termination::User)),
        });

        engine.run_job_inner(&board, &node, &control, 0, 1).unwrap();

        let stopped = repository.node(&board.id, &node.id).unwrap();
        assert_eq!(stopped.status, NodeStatus::Stopped);
        assert_eq!(stopped.stop_reason, Some(StopReason::User));
    }
}
