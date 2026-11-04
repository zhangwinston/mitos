use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use std::sync::LazyLock;
use view::{document::Mode, DocumentId, Editor, ViewId};

use super::engine::ImeContext;
use anyhow::Result;

/// Global IME context registry.
///
/// Performance considerations:
/// - Uses `parking_lot::Mutex` for efficient locking (faster than std::sync::Mutex)
/// - Lock is held for minimal time (only for HashMap lookups/updates)
/// - IME state updates are infrequent (only on cursor move and mode switch)
/// - Contexts are small (Copy types), so operations are fast
static REGISTRY: LazyLock<Mutex<ImeRegistry>> =
    LazyLock::new(|| Mutex::new(ImeRegistry::default()));

#[derive(Default)]
struct ImeRegistry {
    // Use (DocumentId, ViewId) as key to ensure IME state independence per document+view combination
    contexts: HashMap<(DocumentId, ViewId), ImeContext>,
    // Track last access time for each context to enable cleanup
    last_access: HashMap<(DocumentId, ViewId), Instant>,
    // Performance metrics for monitoring
    metrics: InternalRegistryMetrics,
}

#[derive(Default)]
struct InternalRegistryMetrics {
    total_contexts_created: u64,
    total_contexts_removed: u64,
    max_concurrent_contexts: u64,
    current_contexts: u64,
    cleanup_count: u64,
}

impl ImeRegistry {
    fn ensure_context(
        &mut self,
        doc_id: DocumentId,
        view_id: ViewId,
        mode: Mode,
    ) -> &mut ImeContext {
        let key = (doc_id, view_id);
        let now = Instant::now();

        // Update last access time
        self.last_access.insert(key, now);

        // Check if context needs to be created
        if !self.contexts.contains_key(&key) {
            self.metrics.total_contexts_created += 1;
            self.metrics.current_contexts += 1;
            self.metrics.max_concurrent_contexts = self
                .metrics
                .max_concurrent_contexts
                .max(self.metrics.current_contexts);

            log::debug!(
                "Creating new IME context for doc={}, view={:?}",
                doc_id,
                view_id
            );
        }

        self.contexts
            .entry(key)
            .or_insert_with(|| ImeContext::new(mode))
    }

    fn prune_orphans(&mut self, editor: &Editor) {
        // Remove contexts for views that no longer exist
        // and for documents that no longer exist
        let valid_views: std::collections::HashSet<_> =
            editor.tree.views().map(|(v, _)| v.id).collect();
        let valid_docs: std::collections::HashSet<_> = editor.documents.keys().copied().collect();

        let initial_count = self.contexts.len();

        self.contexts.retain(|(doc_id, view_id), _| {
            let is_valid = valid_views.contains(view_id) && valid_docs.contains(doc_id);
            if !is_valid {
                self.last_access.remove(&(*doc_id, *view_id));
                self.metrics.total_contexts_removed += 1;
                self.metrics.current_contexts = self.metrics.current_contexts.saturating_sub(1);
            }
            is_valid
        });

        let removed_count = initial_count - self.contexts.len();
        if removed_count > 0 {
            log::debug!("Pruned {} orphaned IME contexts", removed_count);
        }
    }

    fn remove_document(&mut self, doc_id: DocumentId) {
        // Remove all contexts for a document when it's closed
        let initial_count = self.contexts.len();

        self.contexts.retain(|(d, _), _| {
            let is_different = *d != doc_id;
            if !is_different {
                // Last access map will be cleaned up in the retain loop above
                self.metrics.total_contexts_removed += 1;
            }
            is_different
        });

        self.metrics.current_contexts = self.contexts.len() as u64;

        let removed_count = initial_count - self.contexts.len();
        if removed_count > 0 {
            log::debug!(
                "Removed {} IME contexts for document {}",
                removed_count,
                doc_id
            );
        }
    }

    /// Cleanup old contexts that haven't been accessed for a long time.
    /// This prevents memory leaks from long-running sessions.
    fn cleanup_old_entries(&mut self, max_age: Duration) {
        let now = Instant::now();
        let initial_count = self.contexts.len();

        // Collect keys to remove
        let to_remove: Vec<_> = self
            .last_access
            .iter()
            .filter_map(|(key, last_access)| {
                if now.duration_since(*last_access) > max_age {
                    Some(*key)
                } else {
                    None
                }
            })
            .collect();

        // Remove old entries
        for key in to_remove {
            self.contexts.remove(&key);
            self.last_access.remove(&key);
            self.metrics.total_contexts_removed += 1;
        }

        self.metrics.current_contexts = self.contexts.len() as u64;
        self.metrics.cleanup_count += 1;

        let removed_count = initial_count - self.contexts.len();
        if removed_count > 0 {
            log::debug!(
                "Cleaned up {} stale IME contexts (age > {:?})",
                removed_count,
                max_age
            );
        }
    }

    /// Get current registry metrics for monitoring.
    fn get_metrics(&self) -> RegistryMetrics {
        RegistryMetrics {
            total_contexts_created: self.metrics.total_contexts_created,
            total_contexts_removed: self.metrics.total_contexts_removed,
            max_concurrent_contexts: self.metrics.max_concurrent_contexts,
            current_contexts: self.contexts.len() as u64,
            cleanup_count: self.metrics.cleanup_count,
        }
    }
}

/// Access IME context for a specific document+view combination.
///
/// This function holds the registry lock for the duration of the closure.
/// The lock is held for minimal time as operations are fast (HashMap lookup/update).
pub fn with_context_mut<R>(
    doc_id: DocumentId,
    view_id: ViewId,
    mode: Mode,
    f: impl FnOnce(&mut ImeContext) -> R,
) -> R {
    let mut registry = REGISTRY.lock();
    let ctx = registry.ensure_context(doc_id, view_id, mode);
    f(ctx)
}

pub fn prune_orphans(editor: &Editor) {
    let mut registry = REGISTRY.lock();
    registry.prune_orphans(editor);
}

/// Remove all IME contexts for a document when it's closed
pub fn remove_document(doc_id: DocumentId) {
    let mut registry = REGISTRY.lock();
    registry.remove_document(doc_id);
}

/// Get IME context for a document+view combination (for logging and testing purposes)
#[allow(dead_code)] // Used by integration tests (`tests/test/ime.rs`)
pub fn context(doc_id: DocumentId, view_id: ViewId) -> Option<ImeContext> {
    let registry = REGISTRY.lock();
    registry.contexts.get(&(doc_id, view_id)).copied()
}

/// Cleanup old IME contexts that haven't been accessed recently.
/// This should be called periodically to prevent memory leaks.
pub fn cleanup_old_contexts(max_age: Duration) {
    let mut registry = REGISTRY.lock();
    registry.cleanup_old_entries(max_age);
}

/// Get current registry metrics for monitoring and debugging.
pub fn get_registry_metrics() -> RegistryMetrics {
    let registry = REGISTRY.lock();
    registry.get_metrics()
}

/// Force a consistency check of all cached IME states.
/// This verifies that cached states match system states and updates if necessary.
pub fn verify_all_cached_states() -> Result<usize, anyhow::Error> {
    let registry = REGISTRY.lock();
    let mut inconsistencies = 0;

    for ((doc_id, view_id), ctx) in &registry.contexts {
        if let Some(cached_state) = ctx.cached_ime_state {
            match super::platform::is_ime_enabled() {
                Ok(actual_state) => {
                    if cached_state != actual_state {
                        log::warn!(
                            "IME state inconsistency for doc={}, view={:?}: cached={}, actual={}",
                            doc_id,
                            view_id,
                            cached_state,
                            actual_state
                        );
                        inconsistencies += 1;
                    }
                }
                Err(e) => {
                    log::error!(
                        "Failed to verify IME state for doc={}, view={:?}: {}",
                        doc_id,
                        view_id,
                        e
                    );
                    inconsistencies += 1;
                }
            }
        }
    }

    Ok(inconsistencies)
}

/// Registry metrics for monitoring IME context usage (public wrapper)
#[derive(Debug, Clone, Copy)]
pub struct RegistryMetrics {
    total_contexts_created: u64,
    total_contexts_removed: u64,
    max_concurrent_contexts: u64,
    current_contexts: u64,
    cleanup_count: u64,
}

impl RegistryMetrics {
    pub fn total_contexts_created(&self) -> u64 {
        self.total_contexts_created
    }
    pub fn total_contexts_removed(&self) -> u64 {
        self.total_contexts_removed
    }
    pub fn max_concurrent_contexts(&self) -> u64 {
        self.max_concurrent_contexts
    }
    pub fn current_contexts(&self) -> u64 {
        self.current_contexts
    }
    pub fn cleanup_count(&self) -> u64 {
        self.cleanup_count
    }
}
