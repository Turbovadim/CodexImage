//! The single background writer behind `boards.json`.
//!
//! Board mutations arrive from the GPUI main thread (dragging, renaming,
//! creating boards) and from every generation worker thread at once. Letting
//! each caller serialize and `fsync` its own copy was wrong twice over: the
//! main thread stalled on two `fsync`s per drag-release, and two racing
//! callers could commit out of order, leaving a stale board list on disk.
//!
//! Instead every mutation just bumps a revision. One writer thread serializes
//! the *current* state whenever it is behind, so a burst of updates collapses
//! into a single write and the newest state always lands last.

use super::{RepositoryEvent, RepositoryState, atomic_write};
use crate::model::Board;
use parking_lot::{Condvar, Mutex, RwLock};
use serde::Serialize;
use serde::ser::{SerializeSeq, Serializer};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// How long `flush` waits for the writer before giving up, so a wedged disk
/// cannot keep the app from quitting.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Queue {
    /// The newest revision a caller has asked to see on disk.
    requested: u64,
    /// The newest revision the writer has durably written.
    written: u64,
    /// How many times the file was actually rewritten, which is what makes
    /// request coalescing observable to tests.
    writes: u64,
    shutdown: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    /// Signals the writer that `requested` moved, and waiters that `written` did.
    changed: Condvar,
}

pub(super) struct BoardWriter {
    shared: Arc<Shared>,
    handle: Option<thread::JoinHandle<()>>,
}

impl BoardWriter {
    /// Starts the writer for `boards_file`. Failures are reported through
    /// `events` rather than to the caller that happened to trigger the write,
    /// which by then has usually already returned.
    pub(super) fn spawn(
        state: Arc<RwLock<RepositoryState>>,
        boards_file: PathBuf,
        events: async_channel::Sender<RepositoryEvent>,
    ) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            changed: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("boards-writer".into())
            .spawn(move || run(&worker, &state, &boards_file, &events))
            .ok();
        Self { shared, handle }
    }

    /// Asks for the current state to reach disk. Returns immediately.
    pub(super) fn request(&self) {
        let mut queue = self.shared.queue.lock();
        queue.requested += 1;
        self.shared.changed.notify_all();
    }

    /// How many times the boards file has been rewritten.
    #[cfg(test)]
    pub(super) fn writes(&self) -> u64 {
        self.shared.queue.lock().writes
    }

    /// Blocks until every mutation made so far is durable, or until
    /// `FLUSH_TIMEOUT` elapses. Used on quit and by tests.
    pub(super) fn flush(&self) {
        let mut queue = self.shared.queue.lock();
        // This forced revision is also a synchronization barrier. A mutation
        // may have released the repository write lock but not reached
        // `request` yet; the writer's snapshot will wait for that lock and
        // include the mutation even when shutdown interleaves at that point.
        queue.requested += 1;
        let target = queue.requested;
        self.shared.changed.notify_all();
        while queue.written < target && !queue.shutdown {
            if self
                .shared
                .changed
                .wait_for(&mut queue, FLUSH_TIMEOUT)
                .timed_out()
            {
                return;
            }
        }
    }
}

impl Drop for BoardWriter {
    fn drop(&mut self) {
        {
            let mut queue = self.shared.queue.lock();
            queue.shutdown = true;
            self.shared.changed.notify_all();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run(
    shared: &Shared,
    state: &RwLock<RepositoryState>,
    boards_file: &Path,
    events: &async_channel::Sender<RepositoryEvent>,
) {
    let mut reported_failure = false;
    loop {
        let target = {
            let mut queue = shared.queue.lock();
            while queue.requested == queue.written && !queue.shutdown {
                shared.changed.wait(&mut queue);
            }
            if queue.requested == queue.written {
                return;
            }
            queue.requested
        };

        // Read the state *after* observing `target`, so what lands on disk is
        // never older than the revision that was asked for. Cloning the Arc
        // handles releases the repository lock before the expensive JSON
        // serialization and disk write.
        let boards = state.read().boards.clone();
        let result = serde_json::to_vec_pretty(&BoardSnapshot(&boards))
            .map_err(anyhow::Error::from)
            .and_then(|bytes| atomic_write(boards_file, &bytes));

        {
            let mut queue = shared.queue.lock();
            queue.written = target;
            queue.writes += 1;
            shared.changed.notify_all();
        }

        match result {
            Ok(()) => reported_failure = false,
            // Only the first failure of a run is announced; a full disk would
            // otherwise bury the user under one toast per mutation.
            Err(error) if !reported_failure => {
                reported_failure = true;
                let _ = events.try_send(RepositoryEvent::PersistFailed(error.to_string()));
            }
            Err(_) => {}
        }
    }
}

/// Serializes the pointed-to boards as the same JSON array used on disk. The
/// serde `rc` feature is deliberately unnecessary for this internal wrapper.
struct BoardSnapshot<'a>(&'a [Arc<Board>]);

impl Serialize for BoardSnapshot<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for board in self.0 {
            sequence.serialize_element(board.as_ref())?;
        }
        sequence.end()
    }
}

/// Serializes board-directory deletion onto one detached worker. Deleting a
/// board stays non-blocking for GPUI without creating an unbounded thread for
/// every click.
pub(super) struct CleanupWorker {
    requests: mpsc::Sender<CleanupRequest>,
}

struct CleanupRequest {
    directories: [PathBuf; 3],
    log: PathBuf,
}

impl CleanupWorker {
    pub(super) fn spawn() -> std::io::Result<Self> {
        let (requests, receiver) = mpsc::channel::<CleanupRequest>();
        let _detached = thread::Builder::new()
            .name("storage-cleanup".into())
            .spawn(move || {
                while let Ok(request) = receiver.recv() {
                    for directory in request.directories {
                        let _ = std::fs::remove_dir_all(directory);
                    }
                    let _ = std::fs::remove_file(request.log);
                }
            })?;
        Ok(Self { requests })
    }

    pub(super) fn request(&self, directories: [PathBuf; 3], log: PathBuf) {
        let _ = self.requests.send(CleanupRequest { directories, log });
    }
}

#[cfg(test)]
mod tests {
    use super::{BoardSnapshot, BoardWriter};
    use crate::model::Board;
    use crate::storage::{RepositoryEvent, RepositoryState};
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn state(titles: &[&str]) -> Arc<RwLock<RepositoryState>> {
        Arc::new(RwLock::new(RepositoryState {
            boards: titles
                .iter()
                .map(|title| Board {
                    id: (*title).into(),
                    title: (*title).into(),
                    created_at: 0,
                    nodes: Vec::new(),
                })
                .map(Arc::new)
                .collect(),
            trash: HashMap::new(),
        }))
    }

    /// The writer snapshots the board list at write time, not at request time.
    /// This is what makes the final forced revision safe: it cannot commit a
    /// board list older than a mutation that already holds the state lock.
    #[test]
    fn a_flush_persists_state_mutated_after_the_request() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let boards_file = directory.path().join("boards.json");
        let state = state(&["first"]);
        let (sender, _receiver) = async_channel::unbounded();

        let writer = BoardWriter::spawn(Arc::clone(&state), boards_file.clone(), sender);
        writer.request();
        Arc::make_mut(&mut state.write().boards[0]).title = "second".into();
        writer.flush();

        let written = std::fs::read_to_string(&boards_file).expect("boards file");
        assert!(written.contains("second"), "{written}");
    }

    /// A burst of mutations — a generation writing usage, images, and status
    /// within a few milliseconds — must not become a burst of `fsync`s.
    #[test]
    fn a_burst_of_requests_collapses_into_few_writes() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let boards_file = directory.path().join("boards.json");
        let (sender, _receiver) = async_channel::unbounded();

        let writer = BoardWriter::spawn(state(&["only"]), boards_file, sender);
        for _ in 0..500 {
            writer.request();
        }
        writer.flush();

        assert!(
            writer.writes() < 100,
            "500 requests caused {} writes",
            writer.writes()
        );
    }

    #[test]
    fn arc_snapshot_preserves_the_existing_json_format() {
        let state = state(&["first", "second"]);
        let boards = state.read().boards.clone();
        let actual = serde_json::to_vec_pretty(&BoardSnapshot(&boards)).unwrap();
        let expected_boards: Vec<_> = boards.iter().map(|board| board.as_ref().clone()).collect();
        let expected = serde_json::to_vec_pretty(&expected_boards).unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn write_failures_are_announced_once() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        // A directory where the boards file should be makes every write fail.
        let boards_file = directory.path().join("boards.json");
        std::fs::create_dir(&boards_file).expect("blocking directory");
        let (sender, receiver) = async_channel::unbounded();

        let writer = BoardWriter::spawn(state(&["only"]), boards_file, sender);
        writer.flush();
        writer.request();
        writer.flush();
        drop(writer);

        assert!(matches!(
            receiver.try_recv(),
            Ok(RepositoryEvent::PersistFailed(_))
        ));
        assert!(receiver.try_recv().is_err());
    }
}
