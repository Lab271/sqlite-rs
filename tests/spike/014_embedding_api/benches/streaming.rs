//! Spike 014 (#682): what does the thread boundary cost, and what does
//! streaming buy?
//!
//! Two questions, deliberately separated:
//!
//! * **Full drain** — read every row. Measures the steady-state cost of
//!   one channel hop plus one `SendValue` conversion per row. Streaming
//!   is expected to *lose* here, and by how much is the price of Req-4.
//! * **Time to first row** — read exactly one row of a large result.
//!   This is Req-7's actual claim: a batch caller waits for row N before
//!   seeing row 0.
//!
//! The `Connection` is built once per benchmark, outside the measured
//! closure. An earlier version opened one per iteration and so charged
//! every measurement for a thread spawn and a file open — which is setup
//! a real consumer pays once, not per statement.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use embedding_api_spike::{batch, fixture, Connection};

const ROWS: usize = 50_000;
const SQL: &str = "SELECT id, name, bucket FROM t";

fn full_drain(c: &mut Criterion) {
    let path = fixture(ROWS);
    let conn = Connection::open(&path).expect("open");

    let mut group = c.benchmark_group("full_drain");
    group.throughput(Throughput::Elements(ROWS as u64));

    group.bench_function("batch", |b| {
        b.iter(|| batch::query(&path, SQL, vec![]).expect("batch").len())
    });

    group.bench_function("worker_batch", |b| {
        b.iter(|| conn.query_batch(SQL, vec![]).expect("worker_batch").len())
    });

    group.bench_function("worker_stream", |b| {
        b.iter(|| {
            conn.query_stream(SQL, vec![])
                .expect("worker_stream")
                .count()
        })
    });

    // Where does amortizing the handoff stop paying? Swept rather than
    // guessed, because the whole recommendation rests on this number.
    for chunk in [16_usize, 64, 256, 1024, 4096] {
        group.bench_function(format!("worker_chunked/{chunk}"), |b| {
            b.iter(|| {
                conn.query_chunked(SQL, vec![], chunk)
                    .expect("worker_chunked")
                    .count()
            })
        });
    }

    group.finish();
}

fn time_to_first_row(c: &mut Criterion) {
    let path = fixture(ROWS);
    let conn = Connection::open(&path).expect("open");

    let mut group = c.benchmark_group("time_to_first_row");

    // The control has no way to ask for one row: it builds all 50,000 and
    // the caller takes the first. That is the gap Req-7 describes.
    group.bench_function("batch", |b| {
        b.iter(|| {
            batch::query(&path, SQL, vec![])
                .expect("batch")
                .into_iter()
                .next()
        })
    });

    group.bench_function("worker_stream", |b| {
        b.iter(|| {
            conn.query_stream(SQL, vec![])
                .expect("worker_stream")
                .next()
        })
    });

    // Chunking trades first-row latency for throughput: the caller waits
    // for `chunk` rows, not 1. Both sides of the knee `full_drain` finds
    // are measured, so the trade is read off numbers rather than
    // extrapolated from one of them.
    for chunk in [256_usize, 1024] {
        group.bench_function(format!("worker_chunked/{chunk}"), |b| {
            b.iter(|| {
                conn.query_chunked(SQL, vec![], chunk)
                    .expect("worker_chunked")
                    .next()
            })
        });
    }

    group.finish();
}

criterion_group!(benches, full_drain, time_to_first_row);
criterion_main!(benches);
