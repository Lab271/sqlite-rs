//! Spike 014 (#682): does streaming actually bound memory?
//!
//! Spec 013/Req-7's scenario says a large result must be "read without
//! materializing it". That is a claim about peak heap, so it gets
//! measured rather than asserted. A counting global allocator is the
//! only way to see it from inside the process.
//!
//! This lives in its own test binary on purpose: the counters are
//! process-global, so any concurrently-running test would pollute them.
//! One test per binary, no parallelism to worry about.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use embedding_api_spike::{batch, fixture, Connection};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

// SAFETY: every branch forwards to `System` with the same layout it was
// given; the counters are plain atomics and never touch the returned
// pointers. Permitted here because a spike crate carries its own
// `[workspace]` and so is outside the parent's `unsafe_code = "deny"`.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        System.dealloc(ptr, layout);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The allocation counters are process-global, so two measurements must
/// never overlap. Cargo runs a binary's tests in parallel by default, and
/// relying on `--test-threads=1` would mean plain `cargo test` (what
/// `make test` runs) silently reported garbage. Every test here takes
/// this lock for its whole body.
static MEASURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Arms the peak counter at the current live figure and returns the
/// growth observed while `f` ran.
fn peak_growth<T>(f: impl FnOnce() -> T) -> (usize, T) {
    let start = LIVE.load(Ordering::Relaxed);
    PEAK.store(start, Ordering::Relaxed);
    let out = f();
    let peak = PEAK.load(Ordering::Relaxed);
    (peak.saturating_sub(start), out)
}

/// One fixture for every measurement. An earlier version of this test
/// used a differently-sized database per result size and concluded that
/// streaming's peak "scales with rows" — but it had varied the database
/// and the result set together, so the pager's page cache grew for a
/// reason that had nothing to do with the API. Holding the database
/// fixed and varying only `LIMIT` is what isolates the question.
const FIXTURE_ROWS: usize = 50_000;

/// Measures both prototypes reading `limit` rows out of the same
/// database.
fn measure(path: &std::path::Path, limit: usize) -> (usize, usize) {
    let sql = format!("SELECT id, name, bucket FROM t LIMIT {limit}");

    // Warm the fixture and every lazily-initialized allocation outside
    // the measured window.
    drop(batch::query(path, "SELECT id FROM t WHERE id = 1", vec![]));

    let (batch_peak, batch_rows) =
        peak_growth(|| batch::query(path, &sql, vec![]).expect("batch").len());

    let (stream_peak, stream_rows) = peak_growth(|| {
        let conn = Connection::open(path).expect("open");
        let stream = conn.query_stream(&sql, vec![]).expect("stream");
        // Each row is dropped immediately, so nothing accumulates on the
        // caller's side either; what remains is the engine's own
        // high-water mark.
        let mut seen = 0usize;
        for row in stream {
            let _ = row.expect("row");
            seen += 1;
        }
        seen
    });

    assert_eq!(batch_rows, limit);
    assert_eq!(stream_rows, limit);
    (batch_peak, stream_peak)
}

#[test]
fn streaming_removes_row_accumulation_but_not_page_cache_growth() {
    let _serial = MEASURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = fixture(FIXTURE_ROWS);

    let (small_batch, small_stream) = measure(&path, 10_000);
    let (large_batch, large_stream) = measure(&path, 50_000);

    println!();
    println!("one {FIXTURE_ROWS}-row database, result size varied by LIMIT");
    println!();
    println!("| rows read | batch peak | stream peak | ratio |");
    println!("|----------:|-----------:|------------:|------:|");
    for (rows, b, st) in [
        (10_000, small_batch, small_stream),
        (50_000, large_batch, large_stream),
    ] {
        println!(
            "| {rows} | {b} B | {st} B | {:.1}x |",
            b as f64 / st.max(1) as f64
        );
    }
    let batch_scaling = large_batch as f64 / small_batch.max(1) as f64;
    let stream_scaling = large_stream as f64 / small_stream.max(1) as f64;
    println!();
    println!("batch  peak scaling, 5x rows read: {batch_scaling:.2}x");
    println!("stream peak scaling, 5x rows read: {stream_scaling:.2}x");

    assert!(
        large_stream < large_batch,
        "streaming ({large_stream}) should peak below batch ({large_batch})"
    );
    assert!(
        batch_scaling > 3.0,
        "batch peak should scale with rows read, saw {batch_scaling:.2}x for 5x"
    );
}

/// The decisive experiment for Req-7's "without materializing it".
///
/// The test above shows streaming's peak still growing with rows read,
/// which looks like a failure until you notice why: the 50,000-row
/// fixture is 369 pages and `DEFAULT_PAGE_CACHE_CAPACITY`
/// (`src/pager.rs:63`) is 2000, so the entire database fits in the cache
/// and nothing is ever evicted. Peak tracks pages *touched*, not rows
/// buffered.
///
/// If that reading is right, a database *larger* than the cache must make
/// streaming's peak plateau, because eviction finally engages. That is
/// the difference between "streaming bounds memory" and "streaming just
/// delays the problem", so it gets its own measurement.
#[test]
fn streaming_peak_plateaus_once_the_page_cache_binds() {
    let _serial = MEASURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Sizing this took two attempts, both recorded because the second
    // is only understandable given the first. A 300,000-row fixture is
    // 2,326 pages, apparently past the 2000-page capacity — but its
    // `t_bucket` index owns roughly 700 of those, and a full table scan
    // never touches an index it does not use. Only ~1,630 *table* pages
    // were read, still under the bound, so nothing evicted and the peak
    // kept scaling. 1,000,000 rows puts table leaves alone past 2000.
    const BIG_ROWS: usize = 1_000_000;
    let path = fixture(BIG_ROWS);

    let (batch_small, stream_small) = measure(&path, 400_000);
    let (batch_large, stream_large) = measure(&path, 1_000_000);

    println!();
    println!("one {BIG_ROWS}-row database (past the 2000-page cache bound)");
    println!();
    println!("| rows read | batch peak | stream peak | ratio |");
    println!("|----------:|-----------:|------------:|------:|");
    for (rows, b, st) in [
        (400_000, batch_small, stream_small),
        (1_000_000, batch_large, stream_large),
    ] {
        println!(
            "| {rows} | {b} B | {st} B | {:.1}x |",
            b as f64 / st.max(1) as f64
        );
    }
    let batch_scaling = batch_large as f64 / batch_small.max(1) as f64;
    let stream_scaling = stream_large as f64 / stream_small.max(1) as f64;
    println!();
    println!("batch  peak scaling, 2.5x rows read: {batch_scaling:.2}x");
    println!("stream peak scaling, 2.5x rows read: {stream_scaling:.2}x");
    println!(
        "page-cache ceiling          : {} B (2000 pages x 4096)",
        2000 * 4096
    );

    assert!(
        batch_scaling > 2.0,
        "batch should still scale with rows read, saw {batch_scaling:.2}x for 2.5x"
    );
    assert!(
        stream_scaling < 1.5,
        "streaming should plateau once the cache binds, saw {stream_scaling:.2}x for 2.5x rows"
    );
}
