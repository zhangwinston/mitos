use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::Duration;

#[derive(Default, Debug, Clone, Copy)]
#[cfg(any(test, feature = "integration"))]
pub struct ImeMetricsSnapshot {
    pub cursor_move_calls: u64,
    pub region_detection_calls: u64,
    pub region_cache_hits: u64,
    pub cursor_move_total_time_ns: u64,
    pub non_insert_skips: u64,
    pub invalid_view_skips: u64,
}

struct ImeMetricsInner {
    cursor_move_calls: AtomicU64,
    region_detection_calls: AtomicU64,
    region_cache_hits: AtomicU64,
    cursor_move_total_time_ns: AtomicU64,
    non_insert_skips: AtomicU64,
    invalid_view_skips: AtomicU64,
}

impl Default for ImeMetricsInner {
    fn default() -> Self {
        Self {
            cursor_move_calls: AtomicU64::new(0),
            region_detection_calls: AtomicU64::new(0),
            region_cache_hits: AtomicU64::new(0),
            cursor_move_total_time_ns: AtomicU64::new(0),
            non_insert_skips: AtomicU64::new(0),
            invalid_view_skips: AtomicU64::new(0),
        }
    }
}

static METRICS: LazyLock<ImeMetricsInner> = LazyLock::new(ImeMetricsInner::default);

pub(crate) fn record_cursor_move_call() {
    METRICS.cursor_move_calls.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_cursor_move_skip_not_insert() {
    METRICS.non_insert_skips.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_cursor_move_skip_invalid_view() {
    METRICS.invalid_view_skips.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_region_detection() {
    METRICS
        .region_detection_calls
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_region_cache_hit() {
    METRICS.region_cache_hits.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_cursor_move_duration(duration: Duration) {
    METRICS
        .cursor_move_total_time_ns
        .fetch_add(duration.as_nanos() as u64, Ordering::Relaxed);
}

#[cfg(any(test, feature = "integration"))]
pub fn snapshot() -> ImeMetricsSnapshot {
    ImeMetricsSnapshot {
        cursor_move_calls: METRICS.cursor_move_calls.load(Ordering::Relaxed),
        region_detection_calls: METRICS.region_detection_calls.load(Ordering::Relaxed),
        region_cache_hits: METRICS.region_cache_hits.load(Ordering::Relaxed),
        cursor_move_total_time_ns: METRICS.cursor_move_total_time_ns.load(Ordering::Relaxed),
        non_insert_skips: METRICS.non_insert_skips.load(Ordering::Relaxed),
        invalid_view_skips: METRICS.invalid_view_skips.load(Ordering::Relaxed),
    }
}

#[cfg(any(test, feature = "integration"))]
pub fn reset() {
    METRICS.cursor_move_calls.store(0, Ordering::Relaxed);
    METRICS.region_detection_calls.store(0, Ordering::Relaxed);
    METRICS.region_cache_hits.store(0, Ordering::Relaxed);
    METRICS
        .cursor_move_total_time_ns
        .store(0, Ordering::Relaxed);
    METRICS.non_insert_skips.store(0, Ordering::Relaxed);
    METRICS.invalid_view_skips.store(0, Ordering::Relaxed);
}
