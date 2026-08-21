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
use parking_lot::{Condvar, Mutex, RwLock};
use std::path::PathBuf;
use std::sync::Arc;
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
    boards_file: &PathBuf,
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
        // never older than the revision that was asked for.
        let result = serde_json::to_vec_pretty(&state.read().boards)
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

#[cfg(test)]
mod tests {
    use super::BoardWriter;
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
                .collect(),
            trash: HashMap::new(),
        }))
    }

    /// The writer snapshots the board list at write time, not at request time.
    /// That is what makes concurrent mutations safe: a write already in flight
    /// can never commit a board list older than one a later mutation produced.
    #[test]
    fn a_flush_persists_state_mutated_after_the_request() {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let boards_file = directory.path().join("boards.json");
        let state = state(&["first"]);
        let (sender, _receiver) = async_channel::unbounded();

        let writer = BoardWriter::spawn(Arc::clone(&state), boards_file.clone(), sender);
        writer.request();
        state.write().boards[0].title = "second".into();
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
