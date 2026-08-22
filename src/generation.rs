mod codex;
pub(crate) mod conditioner;
mod prompt;

pub use conditioner::condition_image_for_reingestion;

use crate::manifest::{OutputManifest, absolute_file_path, is_path_inside};
use crate::model::{
    Board, BoardNode, MAX_ACTIVE_PER_BOARD, NewNodesRequest, NodeStatus, StopReason,
};
use crate::storage::{Repository, now_ms};
use anyhow::{Context, Result, bail};
use codex::{CodexInvocation, configure_process_group, kill_process_group, read_tail};
use parking_lot::Mutex;
use prompt::{build_node_prompt, selection_recovery_prompt, tail_chars};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
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

#[derive(Clone)]
pub struct GenerationEngine {
    inner: Arc<EngineInner>,
}

struct EngineInner {
    repository: Repository,
    jobs: Mutex<HashMap<String, Arc<JobControl>>>,
    submission_lock: Mutex<()>,
    codex: OnceLock<CodexInvocation>,
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
                submission_lock: Mutex::new(()),
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
        let _submission = self.inner.submission_lock.lock();
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
        let _submission = self.inner.submission_lock.lock();
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
        self.inner.repository.flush();
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
        let source_paths: Vec<_> = node
            .source_images
            .iter()
            .filter_map(|url| self.inner.repository.image_path(&board.id, url))
            .filter(|path| path.exists())
            .collect();
        let reingest_directory = workspace
            .join("reingest")
            .join(&node.id)
            .join(node.run_started_at.unwrap_or(node.created_at).to_string());
        let source_paths = conditioner::prepare_source_images(&source_paths, &reingest_directory);
        let same_run_conditioner = if conditioner::enabled() {
            std::env::current_exe().ok().and_then(|executable| {
                let directory = reingest_directory.join("same-run");
                fs::create_dir_all(&directory)
                    .ok()
                    .map(|()| (executable, directory))
            })
        } else {
            None
        };
        if control.termination.lock().is_some() {
            return self.finalize(board, node, control, &mut Runtime::new());
        }
        let prompt = build_node_prompt(
            &self.inner.repository,
            board,
            node,
            &source_paths,
            same_run_conditioner
                .as_ref()
                .map(|(executable, directory)| (executable.as_path(), directory.as_path())),
            index,
            count,
        );
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
        // A replaced job is finalized by the run that replaced it.
        if termination == Some(Termination::Replaced) {
            return Ok(());
        }
        // Keep whatever text the agent produced so it stays visible even when
        // the run failed, timed out, or was stopped: the manifest summary when
        // the message parses, the raw message otherwise.
        if let Some(message) = runtime.last_agent_message.as_deref() {
            let text = match crate::manifest::parse(message) {
                Ok(manifest) => manifest.summary,
                Err(_) => message.trim().chars().take(20_000).collect(),
            };
            if !text.is_empty() {
                self.inner
                    .repository
                    .update_node(&board.id, &node.id, |node| {
                        if node.status == NodeStatus::Running {
                            node.text = text;
                        }
                    })?;
            }
        }
        if let Some(termination) = termination {
            if let Some(reason) = termination.stop_reason() {
                return self
                    .inner
                    .repository
                    .mark_stopped(&board.id, &node.id, reason);
            }
            return self.record_timeout(board, node);
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

#[cfg(test)]
mod tests {
    use super::{GenerationEngine, JobControl, Termination};
    use crate::model::{NewNodesRequest, NodeStatus, StopReason};
    use crate::storage::{DataPaths, Repository};
    use async_channel::unbounded;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use std::sync::atomic::AtomicI32;
    use tempfile::TempDir;

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
                    position: None,
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
