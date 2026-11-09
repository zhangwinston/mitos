//! IME (Input Method Editor) automatic control handler.
//!
//! This module provides automatic IME state management based on cursor position
//! and editor mode. IME is automatically enabled in string/comment regions and
//! disabled in code regions.

pub mod platform;

pub mod engine;
pub mod metrics;
#[cfg(any(test, feature = "integration"))]
pub mod registry;
#[cfg(not(any(test, feature = "integration")))]
mod registry;
mod scheduler;

use anyhow::Result;
use event::register_hook;
use language::syntax::{detect_ime_sensitive_region, ImeSensitiveRegion};
use std::sync::{Once, OnceLock};
use std::time::{Duration, Instant};
use view::{
    document::{Document, Mode},
    events::{DocumentDidClose, DocumentDidOpen, ModeDidChange, SelectionDidChange},
    DocumentId, Editor, ViewId,
};

use crate::handlers::ime::platform::{
    initialize, is_ime_enabled, set_ime_enabled, ImeDetector, ImeSettings,
};
use engine::{is_sensitive, ImeContext, ImeEngine, ImeRegionSpan};

/// Cached retry parameters derived once from [`platform::get_ime_info`].
///
/// Avoids calling `get_ime_info` on every IME state read (hot path during cursor moves).
static IME_RETRY_SETTINGS: OnceLock<ImeSettings> = OnceLock::new();

fn ime_retry_settings() -> &'static ImeSettings {
    IME_RETRY_SETTINGS.get_or_init(|| match platform::get_ime_info() {
        Ok(ime_info) => {
            log::info!(
                "Detected IME: {} (capabilities: {:?})",
                ime_info.name,
                ime_info.capabilities
            );
            let ime_type = ImeDetector::detect_ime_type(&ime_info.name);
            ImeDetector::get_optimal_settings(ime_type)
        }
        Err(e) => {
            log::warn!("Failed to get IME info: {}", e);
            ImeSettings::default()
        }
    })
}

/// One-shot guard for process-global IME initialization.
///
/// Unlike helix (where handler setup runs once per process), mitos installs
/// presentation hooks per editor: every `Application` — including each
/// parallel integration test — would otherwise run `platform::initialize`,
/// re-derive the retry settings and spawn another cleanup task. The state
/// behind all of those is process-global, so only the first call does work;
/// later calls log and swallow errors.
static IME_SUPPORT_INIT: Once = Once::new();

/// Initialize IME support when the application starts.
/// This should be called early in the application lifecycle.
pub fn initialize_ime_support() -> Result<()> {
    let mut result = Ok(());
    IME_SUPPORT_INIT.call_once(|| {
        result = (|| -> Result<()> {
            initialize()?;
            log::info!("IME support initialized successfully");

            // Warm retry-setting cache and log detected IME (single `get_ime_info` call).
            let _ = ime_retry_settings();

            // Start cleanup task
            start_cleanup_task();

            Ok(())
        })();
    });
    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            log::error!("Failed to initialize IME support: {}", e);
            Err(e)
        }
    }
}

/// Get the primary cursor byte position from a document and view.
///
/// This function extracts the cursor position from the primary selection
/// and converts it to a byte offset for use in region detection.
///
/// # Arguments
/// * `doc` - The document containing the text
/// * `view_id` - The view ID to get the selection from
///
/// # Returns
/// The cursor position as a byte offset
fn get_cursor_byte_pos(doc: &Document, view_id: ViewId) -> usize {
    let text = doc.text().slice(..);
    let selection = doc.selection(view_id);
    let primary_selection = selection.primary();
    let cursor_pos = primary_selection.cursor(text);
    text.char_to_byte_idx(cursor_pos)
}

/// Update the cached IME state in the registry.
///
/// This function updates the cached IME state after a successful state change
/// or when the cache needs to be refreshed (e.g., after detecting a manual toggle).
///
/// # Arguments
/// * `doc_id` - Document ID for cache lookup
/// * `view_id` - View ID for cache lookup
/// * `mode` - Current editor mode
/// * `new_state` - The new IME state to cache
fn update_ime_cache(doc_id: DocumentId, view_id: ViewId, mode: Mode, new_state: bool) {
    registry::with_context_mut(doc_id, view_id, mode, |ctx| {
        ctx.cached_ime_state = Some(new_state);
    });
}

/// Initialize IME context for a document+view combination.
///
/// This function resets the ImeContext and closes IME if it's currently enabled.
/// Called when a view is created or a document is opened.
pub fn initialize_view_ime_state(editor: &mut Editor, view_id: ViewId) {
    if !editor.config().ime_auto_control {
        return;
    }

    if !editor.tree.contains(view_id) {
        return;
    }

    let doc_id = editor.tree.get(view_id).document_id();
    if let Some(doc) = editor.documents.get_mut(&doc_id) {
        doc.ensure_view_init(view_id);
    }

    // Reset IME context for this document+view combination
    registry::with_context_mut(doc_id, view_id, editor.mode(), |ctx| {
        ctx.reset(editor.mode())
    });

    // Close IME if it's currently enabled (FR-001)
    // Query system state during initialization (cache may not be valid yet)
    let ime_was_enabled = read_ime_enabled("view initialization");
    if ime_was_enabled {
        if let Err(e) = set_ime_enabled(false) {
            log::error!("Failed to close IME during view initialization: {}", e);
        } else {
            // Update cache after successful state change
            update_ime_cache(doc_id, view_id, editor.mode(), false);
            return;
        }
    }

    // Update cache even if IME was already disabled (or we failed to close it)
    update_ime_cache(doc_id, view_id, editor.mode(), false);
}

/// If the cursor lies in [`ImeContext::cached_region_span`] for `doc_version`, returns that span's region.
/// Clears a stale span (version or position mismatch).
fn span_region_at_cursor(
    ctx: &mut ImeContext,
    doc_version: i32,
    cursor_byte_pos: usize,
) -> Option<ImeSensitiveRegion> {
    if let Some(span) = ctx.cached_region_span {
        if span.doc_version == doc_version
            && cursor_byte_pos >= span.start
            && cursor_byte_pos < span.end
        {
            return Some(span.region);
        }
        ctx.cached_region_span = None;
    }
    None
}

/// Snapshot of IME-related registry fields read together with region resolution.
type ImeCursorSnap = (Option<bool>, Option<ImeSensitiveRegion>, bool);

fn ime_cursor_snap(ctx: &ImeContext) -> ImeCursorSnap {
    (
        ctx.cached_ime_state,
        ctx.current_region,
        ctx.current_region.map(is_sensitive).unwrap_or(false),
    )
}

/// Handle cursor movement and update IME state accordingly.
///
/// This function detects the IME-sensitive region at the cursor position and
/// updates IME state if the region has changed.
///
/// # Optimization for EntireFile regions
/// For files without string/comment types (e.g., TOML, YAML), the entire file
/// is IME-sensitive but the region type never changes with the cursor, so there is
/// nothing to do on [`SelectionDidChange`]. IME open/close and restore are handled
/// on mode switch; entering Insert re-reads the system IME state for this case.
///
/// # Lock Consolidation
/// Registry locks are consolidated where possible to reduce contention:
/// - First two locks (get current_region for EntireFile check, then get snapshot) are merged
/// - Third lock for cache miss remains separate (needs detection result)
/// - Fourth lock for engine work remains separate (needs mutable context)
pub fn handle_cursor_move(editor: &mut Editor, view_id: ViewId) -> Result<()> {
    // Feature disabled: skip before touching any global state.
    if !editor.config().ime_auto_control {
        return Ok(());
    }

    metrics::record_cursor_move_call();

    // Only process in Insert mode (FR-002)
    if editor.mode() != Mode::Insert {
        metrics::record_cursor_move_skip_not_insert();
        return Ok(());
    }

    if !editor.tree.contains(view_id) {
        metrics::record_cursor_move_skip_invalid_view();
        return Ok(());
    }

    // No need to prune orphans on every cursor move - it's expensive
    // Orphans are cleaned up on DocumentDidClose and DocumentDidOpen events

    let timer = Instant::now();
    let editor_mode = editor.mode();
    let doc_id = editor.tree.get(view_id).document_id();

    let doc = match editor.documents.get_mut(&doc_id) {
        Some(doc) => doc,
        None => {
            metrics::record_cursor_move_duration(timer.elapsed());
            return Err(anyhow::anyhow!("Document not found"));
        }
    };
    doc.ensure_view_init(view_id);
    let doc_version = doc.version();

    // Get primary cursor position (FR-020)
    let cursor_byte_pos = get_cursor_byte_pos(doc, view_id);

    // ------------------------------------------------------------
    // MERGED LOCK: Get EntireFile check + snapshot in single lock
    // (was: first lock for current_region, second lock for snapshot)
    // ------------------------------------------------------------
    let (current_region, new_region_or, record_span_hit, ime_snap_opt) =
        registry::with_context_mut(doc_id, view_id, editor_mode, |ctx| {
            // EntireFile check
            let current_region = ctx.current_region;

            // Compute snapshot data
            let cursor_changed = ctx.cached_cursor_byte_pos != Some(cursor_byte_pos);
            let region_unknown = ctx.current_region.is_none();
            let needs_detection = cursor_changed || region_unknown;
            if needs_detection {
                ctx.cached_cursor_byte_pos = Some(cursor_byte_pos);
            }

            if !needs_detection {
                let span_hit = span_region_at_cursor(ctx, doc_version, cursor_byte_pos).is_some();
                let region = ctx.current_region.unwrap_or(ImeSensitiveRegion::Code);
                let snap = ime_cursor_snap(ctx);
                return (current_region, Some(region), span_hit, Some(snap));
            }

            if let Some(r) = span_region_at_cursor(ctx, doc_version, cursor_byte_pos) {
                let snap = ime_cursor_snap(ctx);
                return (current_region, Some(r), true, Some(snap));
            }

            (current_region, None, false, None)
        });

    // EntireFile: region does not depend on cursor — no auto-switch on cursor move.
    // System IME is read when entering Insert (see `handle_mode_switch`).
    if current_region == Some(ImeSensitiveRegion::EntireFile) {
        metrics::record_region_cache_hit();
        metrics::record_cursor_move_duration(timer.elapsed());
        log::trace!(
            "IME: Skipping cursor-move handling for EntireFile region (doc={:?}, view={:?})",
            doc_id,
            view_id
        );
        return Ok(());
    }

    let (new_region, (cached_ime_state, current_region, is_sensitive_region)) =
        match (new_region_or, ime_snap_opt) {
            (Some(r), Some(snap)) => {
                if record_span_hit {
                    metrics::record_region_cache_hit();
                }
                (r, snap)
            }
            (None, None) => {
                metrics::record_region_detection();
                let text = doc.text().slice(..);
                let syntax = doc.syntax();
                let loader = doc.syntax_loader();
                let detection = detect_ime_sensitive_region(syntax, text, loader, cursor_byte_pos);
                let region = detection.region;
                let snap = registry::with_context_mut(doc_id, view_id, editor_mode, |ctx| {
                    ctx.cached_region_span = detection.node_range.and_then(|(start, end)| {
                        (start < end).then_some(ImeRegionSpan {
                            doc_version,
                            start,
                            end,
                            region,
                        })
                    });
                    ime_cursor_snap(ctx)
                });
                (region, snap)
            }
            _ => unreachable!("IME cursor-move: inconsistent region cache vs snapshot"),
        };

    // Query system IME state:
    // 1. If cache is empty (cache miss)
    // 2. If we're in a sensitive region and region hasn't changed (user may have manually toggled IME)
    //    In this case, we need to verify the cache is still valid
    let region_unchanged = current_region == Some(new_region);
    let is_new_region_sensitive = is_sensitive(new_region);
    let should_verify_cache = cached_ime_state.is_some()
        && is_sensitive_region
        && is_new_region_sensitive
        && region_unchanged;

    // Leaving a sensitive region: the state is about to be saved for later
    // restore, so it must be read fresh — the user may have toggled the IME
    // manually while inside the sensitive region, and restoring a stale
    // cached value would lose that (only *staying* inside a sensitive region
    // used to trigger a fresh read).
    let leaving_sensitive = is_sensitive_region && !is_new_region_sensitive;
    let current_ime_enabled = if should_verify_cache || leaving_sensitive {
        // Region unchanged in sensitive area: verify cache by querying system
        read_ime_enabled("cursor move (cache verification)")
    } else {
        // Cache miss or region changed: use cache if available, otherwise query system
        cached_ime_state.unwrap_or_else(|| read_ime_enabled("cursor move"))
    };

    // Sync cache on mismatch (same lock as engine to avoid an extra registry pass)
    let target_state = registry::with_context_mut(doc_id, view_id, editor_mode, |ctx| {
        if should_verify_cache && ctx.cached_ime_state != Some(current_ime_enabled) {
            log::trace!(
                "IME: Cache mismatch detected (cached={:?}, system={}), updating cache",
                ctx.cached_ime_state,
                current_ime_enabled
            );
            ctx.cached_ime_state = Some(current_ime_enabled);
        }
        let mut engine = ImeEngine::new(ctx);
        engine.on_region_change(new_region, current_ime_enabled)
    });

    // Execute system API call (outside lock to avoid blocking other operations)
    if let Some(target) = target_state {
        if let Err(e) = set_ime_enabled(target) {
            log::error!("Failed to toggle IME on cursor move: {}", e);
        } else {
            // Update cache after successful state change
            update_ime_cache(doc_id, view_id, editor_mode, target);
        }
    }

    metrics::record_cursor_move_duration(timer.elapsed());
    Ok(())
}

/// Handle mode switch and update IME state accordingly.
///
/// This function saves IME state when exiting Insert mode and restores it
/// when entering Insert mode (if cursor is in a sensitive region).
pub fn handle_mode_switch(
    editor: &mut Editor,
    view_id: ViewId,
    old_mode: Mode,
    new_mode: Mode,
) -> Result<()> {
    // Feature disabled: skip before touching any global state.
    if !editor.config().ime_auto_control {
        return Ok(());
    }

    if !editor.tree.contains(view_id) {
        return Ok(());
    }

    // No need to prune orphans on every mode switch - it's expensive
    // Orphans are cleaned up on DocumentDidClose and DocumentDidOpen events

    let doc_id = editor.tree.get(view_id).document_id();

    if old_mode == Mode::Insert {
        // Always query the actual IME state when exiting Insert mode, don't rely on cache.
        // This is important because the user may have manually enabled IME during Insert mode,
        // and the cache might be stale (e.g., set to false during initialization).
        let current_ime_enabled = read_ime_enabled("mode switch exit");

        // Check if we need to detect the region (if current_region is None)
        let needs_region_detection = registry::with_context_mut(doc_id, view_id, old_mode, |ctx| {
            ctx.current_region.is_none()
        });

        // If current_region is None, detect it before exiting Insert mode
        // This ensures we can correctly decide whether to save the IME state
        if needs_region_detection {
            let doc = editor
                .documents
                .get_mut(&doc_id)
                .ok_or_else(|| anyhow::anyhow!("Document not found"))?;
            doc.ensure_view_init(view_id);

            let cursor_byte_pos = get_cursor_byte_pos(doc, view_id);

            // Get syntax tree and loader
            let text = doc.text().slice(..);
            let syntax = doc.syntax();
            let loader = doc.syntax_loader();

            // Detect IME sensitive region
            metrics::record_region_detection();
            let detection = detect_ime_sensitive_region(syntax, text, loader, cursor_byte_pos);

            // Update context with detected region
            registry::with_context_mut(doc_id, view_id, old_mode, |ctx| {
                ctx.current_region = Some(detection.region);
            });
        }

        // Decide what action to take (in lock, fast)
        let target_state = registry::with_context_mut(doc_id, view_id, old_mode, |ctx| {
            let mut engine = ImeEngine::new(ctx);
            engine.on_exit_insert(current_ime_enabled)
        });

        // Execute system API call (outside lock to avoid blocking other operations)
        if let Some(target) = target_state {
            if let Err(e) = set_ime_enabled(target) {
                log::error!("Failed to close IME when exiting Insert mode: {}", e);
            } else {
                // Update cache after successful state change
                update_ime_cache(doc_id, view_id, old_mode, target);
            }
        }
    }

    registry::with_context_mut(doc_id, view_id, new_mode, |ctx| ctx.mode = new_mode);

    if new_mode == Mode::Insert {
        // Entering Insert mode: restore IME state if cursor is in sensitive region (FR-002, FR-013, FR-014)
        let doc = editor
            .documents
            .get_mut(&doc_id)
            .ok_or_else(|| anyhow::anyhow!("Document not found"))?;
        doc.ensure_view_init(view_id);

        let cursor_byte_pos = get_cursor_byte_pos(doc, view_id);

        // Update cached cursor position
        registry::with_context_mut(doc_id, view_id, new_mode, |ctx| {
            ctx.cached_cursor_byte_pos = Some(cursor_byte_pos);
        });
        let doc_version = doc.version();

        // Get syntax tree and loader
        let text = doc.text().slice(..);
        let syntax = doc.syntax();
        let loader = doc.syntax_loader();

        // Detect IME sensitive region (always detect on mode switch)
        metrics::record_region_detection();
        let detection = detect_ime_sensitive_region(syntax, text, loader, cursor_byte_pos);
        registry::with_context_mut(doc_id, view_id, new_mode, |ctx| {
            ctx.cached_region_span = detection.node_range.and_then(|(start, end)| {
                (start < end).then_some(ImeRegionSpan {
                    doc_version,
                    start,
                    end,
                    region: detection.region,
                })
            });
        });

        // Decide what action to take (in lock, fast)
        // When saved_state exists, always read the actual system state to determine if we need
        // to change IME state. The cached state might be stale if the user manually toggled IME.
        let target_state = registry::with_context_mut(doc_id, view_id, new_mode, |ctx| {
            // First check if we have saved_state - if so, we need to re-read system state
            let has_saved_state = ctx.saved_state.is_some();

            // Determine the current_ime_enabled based on whether we have saved state
            // We must determine this BEFORE creating the engine to avoid borrow conflicts
            let current_ime_enabled = if has_saved_state {
                // Re-read system state since saved_state exists
                // This handles the case where user manually toggled IME after our last update
                read_ime_enabled("mode switch (with saved state)")
            } else {
                // No saved state, use the cached value
                // For EntireFile, always use system state; for others, use cache or fallback
                if detection.region == ImeSensitiveRegion::EntireFile {
                    read_ime_enabled("mode switch (entire file)")
                } else {
                    ctx.cached_ime_state
                        .unwrap_or_else(|| read_ime_enabled("mode switch"))
                }
            };

            // Now create engine and get the decision - all ctx access must happen here
            let mut engine = ImeEngine::new(ctx);
            let decision = engine.on_enter_insert(detection.region, current_ime_enabled);

            // Update cache with the system state we just read
            ctx.cached_ime_state = Some(current_ime_enabled);

            decision
        });

        // Execute system API call (outside lock to avoid blocking other operations)
        if let Some(target) = target_state {
            log::trace!(
                "IME: mode switch entering insert, calling set_ime_enabled({})",
                target
            );
            if let Err(e) = set_ime_enabled(target) {
                log::error!(
                    "Failed to restore IME state when entering Insert mode: {}",
                    e
                );
            } else {
                // Update cache after successful state change
                update_ime_cache(doc_id, view_id, new_mode, target);
            }
        } else {
            let saved =
                registry::with_context_mut(doc_id, view_id, new_mode, |ctx| ctx.saved_state);
            log::trace!(
                "IME: mode switch entering insert, no action (saved_state={:?})",
                saved
            );
        }
    }

    Ok(())
}

/// Start the background cleanup task for IME contexts.
/// This task runs periodically to clean up stale contexts and prevent memory leaks.
pub fn start_cleanup_task() {
    // Spawn a background task that runs cleanup every 5 minutes
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300)); // 5 minutes

        loop {
            interval.tick().await;

            // Clean up contexts that haven't been accessed for 30 minutes
            registry::cleanup_old_contexts(Duration::from_secs(1800));

            // Log metrics
            let metrics = registry::get_registry_metrics();
            log::debug!(
                "IME registry metrics: current={}, created={}, removed={}, max={}, cleanups={}",
                metrics.current_contexts(),
                metrics.total_contexts_created(),
                metrics.total_contexts_removed(),
                metrics.max_concurrent_contexts(),
                metrics.cleanup_count()
            );

            // Periodic consistency check every hour
            if metrics.cleanup_count().is_multiple_of(12) {
                match registry::verify_all_cached_states() {
                    Ok(0) => log::debug!("All IME cached states are consistent"),
                    Ok(count) => log::warn!("Found {} inconsistent IME states", count),
                    Err(e) => log::error!("Failed to verify IME states: {}", e),
                }
            }
        }
    });
}

/// Register IME handler event hooks.
pub fn register_hooks(events: &event::EventHub) {
    // Initialize IME support first
    if let Err(e) = initialize_ime_support() {
        log::warn!("IME support initialization failed: {}", e);
    }

    // Register hook for view initialization (DocumentDidOpen)
    register_hook!(events => move |event: &mut DocumentDidOpen<'_>| {
        // Collect view IDs that use this document first to avoid borrow conflicts
        let view_ids: Vec<_> = event
            .editor
            .tree
            .views()
            .filter_map(|(view, _is_focused)| {
                if view.document_id() == event.doc {
                    Some(view.id)
                } else {
                    None
                }
            })
            .collect();

        // Initialize IME state for all views that use this document
        for view_id in view_ids {
            initialize_view_ime_state(event.editor, view_id);
        }
        Ok(())
    });

    register_hook!(events => move |event: &mut DocumentDidClose<'_>| {
        // Feature disabled: skip before touching the global registry.
        if !event.editor.config().ime_auto_control {
            return Ok(());
        }

        // Remove all IME contexts for the closed document
        registry::remove_document(event.doc.id());
        registry::prune_orphans(event.editor);
        Ok(())
    });

    // Register hook for mode changes (view-level ModeDidChange).
    //
    // Mode switches must reach the IME controller through `Editor::set_mode`'s
    // own event: it is the single choke point every transition passes through,
    // emitting exactly once per actual change. The term-level `OnModeSwitch`
    // only observes net changes around keymap-driven commands, so a custom
    // command sequence step like `normal_mode` — especially when resumed from
    // a deferred `%sh{}` expansion — could switch modes without the exit-insert
    // bookkeeping ever running, leaving the IME enabled in Normal mode.
    register_hook!(events => move |event: &mut ModeDidChange<'_>| {
        // Get current view ID
        let view_id = event.editor.tree.focus();

        // Handle mode switch for IME state update
        if let Err(e) = handle_mode_switch(event.editor, view_id, event.old, event.new) {
            // Silently fail and log errors (FR-019)
            log::error!("Failed to handle mode switch for IME: {}", e);
        }
        Ok(())
    });
}

/// Register the cursor-move hook (SelectionDidChange) on this application's editor.
///
/// `SelectionDidChange` carries no `&mut Editor`, but the debounced worker
/// needs a [`crate::job::JobSender`] to dispatch back onto the application's
/// event loop, so this hook is registered per editor (from
/// [`crate::handlers::bind_editor`]) rather than with the hub-level hooks.
pub fn bind_editor(editor: &view::Editor, jobs: crate::job::JobSender) {
    register_hook!(editor.events() => move |event: &mut SelectionDidChange<'_>| {
        scheduler::schedule(event.view, jobs.clone());
        Ok(())
    });
}

/// Helper to query IME state with comprehensive error logging and retry mechanism.
/// Always returns a boolean fallback to keep editor responsive (FR-019).
fn read_ime_enabled(context: &str) -> bool {
    match read_ime_enabled_with_retry(context) {
        Ok(state) => state,
        Err(e) => {
            log::error!(
                "Failed to read IME state during {} after retries: {}",
                context,
                e
            );
            false // Fallback to disabled to avoid unexpected IME behavior
        }
    }
}

/// Query IME state with retry mechanism and platform-specific settings.
///
/// This function will retry using settings optimized for the detected IME type.
///
/// Retries use [`std::thread::yield_now`] instead of sleeping: `read_ime_enabled` runs on
/// blocking job threads (e.g. IME cursor handler), and millisecond sleeps would stall that pool.
fn read_ime_enabled_with_retry(context: &str) -> Result<bool> {
    let settings = ime_retry_settings();

    let mut last_error = None;

    for attempt in 1..=settings.retry_count {
        match is_ime_enabled() {
            Ok(state) => {
                if attempt > 1 {
                    log::debug!("IME state query succeeded on attempt {}", attempt);
                }
                return Ok(state);
            }
            Err(e) => {
                last_error = Some(e);

                // Check if this is a temporary error that should be retried
                if is_temporary_ime_error(last_error.as_ref().unwrap())
                    && attempt < settings.retry_count
                {
                    log::debug!(
                        "Temporary IME error during {} (attempt {}/{}), retrying: {}",
                        context,
                        attempt,
                        settings.retry_count,
                        last_error.as_ref().unwrap()
                    );
                    // Cooperate with the scheduler without parking this thread for tens of ms.
                    std::thread::yield_now();
                    continue;
                }

                // Permanent error or max retries reached
                log::error!(
                    "Failed to read IME state during {} after {} attempts: {}",
                    context,
                    attempt,
                    last_error.as_ref().unwrap()
                );
                break;
            }
        }
    }

    Err(last_error.unwrap())
}

/// Determine if an IME error is temporary and should be retried.
///
/// Temporary errors might include:
/// - System resource temporarily unavailable
/// - IME initialization in progress
/// - Window focus transitions
fn is_temporary_ime_error(error: &anyhow::Error) -> bool {
    let error_str = error.to_string().to_lowercase();

    // Check for common temporary error patterns
    error_str.contains("temporarily unavailable")
        || error_str.contains("try again")
        || error_str.contains("in progress")
        || error_str.contains("busy")
        || error_str.contains("timeout")
}

/// Verify IME state consistency between cache and system.
///
/// This function checks if the cached IME state matches the actual system state.
/// If there's a mismatch, it updates the cache and returns false.
/// Returns true if states are consistent.
#[allow(dead_code)]
pub fn verify_ime_state_consistency(editor: &Editor, view_id: ViewId) -> Result<bool> {
    if !editor.tree.contains(view_id) {
        return Ok(false);
    }

    let doc_id = editor.tree.get(view_id).document_id();
    let mode = editor.mode();

    registry::with_context_mut(doc_id, view_id, mode, |ctx| {
        if let Some(cached_state) = ctx.cached_ime_state {
            // Verify system state
            match read_ime_enabled_with_retry("state verification") {
                Ok(actual_state) => {
                    if cached_state != actual_state {
                        log::warn!(
                            "IME state inconsistency detected (context verification): cached={}, actual={}",
                            cached_state, actual_state
                        );
                        // Don't update cache here, just report inconsistency
                        // The caller can decide whether to update
                        Ok(false)
                    } else {
                        Ok(true)
                    }
                }
                Err(e) => {
                    log::error!("Failed to verify IME state consistency: {}", e);
                    // Assume inconsistent if we can't verify
                    Ok(false)
                }
            }
        } else {
            // No cached state, so not inconsistent
            Ok(true)
        }
    })
}
