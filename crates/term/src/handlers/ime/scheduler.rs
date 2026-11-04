use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use dashmap::DashMap;
use tokio::time::sleep;
use view::ViewId;

use super::handle_cursor_move;
use crate::job::JobSender;

const CURSOR_MOVE_BUFFER: Duration = Duration::from_millis(50);

// Process-global pending map (helix keyed this per tokio-runtime via
// helix-event's runtime_local, which mitos has no equivalent of): every
// integration test builds its own Editor with colliding ViewIds, and a worker
// from one runtime must not consume another runtime's cursor moves — or
// process them against the wrong editor. In mitos the mitigation is the IME
// test lock (IME_TEST_LOCK in tests) serializing IME tests plus the default
// `ime_auto_control = false` test config, which keeps every non-IME test from
// scheduling at all. In production this is a plain process-global static (one
// Editor per process). The LazyLock wrapper keeps the initializer const.
static PENDING_VIEWS: LazyLock<DashMap<ViewId, Arc<PendingState>>> =
    LazyLock::new(DashMap::default);

struct PendingState {
    sequence: AtomicU64,
    worker_running: AtomicBool,
    /// Handle of the live worker, used to detect workers that died without
    /// resetting `worker_running` (task cancelled mid-debounce by a runtime
    /// shutdown or a panic — after which cursor-move handling for this view
    /// would otherwise silently stop forever).
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl PendingState {
    fn new() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            worker_running: AtomicBool::new(false),
            worker: Mutex::new(None),
        }
    }
}

pub(super) fn schedule(view_id: ViewId, jobs: JobSender) {
    let state = PENDING_VIEWS
        .entry(view_id)
        .or_insert_with(|| Arc::new(PendingState::new()))
        .clone();

    state.sequence.fetch_add(1, Ordering::Release);

    // Take over from a cancelled worker: its task never ran the cleanup that
    // resets the flag, but a finished JoinHandle proves it is dead.
    let worker_dead = state
        .worker
        .lock()
        .map(|handle| handle.as_ref().is_some_and(|handle| handle.is_finished()))
        .unwrap_or(false);
    if worker_dead {
        state.worker_running.store(false, Ordering::Release);
    }

    if state.worker_running.swap(true, Ordering::AcqRel) {
        return;
    }

    let handle = spawn_worker(view_id, state.clone(), jobs);
    *state.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
}

pub(super) fn cancel(view_id: ViewId) {
    if let Some((_, state)) = PENDING_VIEWS.remove(&view_id) {
        state.worker_running.store(false, Ordering::Release);
    }
}

fn spawn_worker(
    view_id: ViewId,
    state: Arc<PendingState>,
    jobs: JobSender,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let observed_seq = state.sequence.load(Ordering::Acquire);

            sleep(CURSOR_MOVE_BUFFER).await;

            if state.sequence.load(Ordering::Acquire) != observed_seq {
                continue;
            }

            let view_alive = Arc::new(AtomicBool::new(true));
            let view_alive_flag = view_alive.clone();
            let view_id_copy = view_id;
            jobs.send_blocking(move |_, cx| {
                let editor = &mut *cx.editor;
                if !editor.tree.contains(view_id_copy) {
                    view_alive_flag.store(false, Ordering::Release);
                    return;
                }

                if let Err(e) = handle_cursor_move(editor, view_id_copy) {
                    log::error!("Failed to handle cursor move for IME: {}", e);
                }
            });

            if !view_alive.load(Ordering::Acquire) {
                state.worker_running.store(false, Ordering::Release);
                cancel(view_id);
                break;
            }

            if state.sequence.load(Ordering::Acquire) == observed_seq
                && state
                    .worker_running
                    .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                // Check if new events arrived after releasing the flag.
                if state.sequence.load(Ordering::Acquire) == observed_seq {
                    break;
                }

                if state.worker_running.swap(true, Ordering::AcqRel) {
                    break;
                }

                continue;
            }
        }
    })
}
