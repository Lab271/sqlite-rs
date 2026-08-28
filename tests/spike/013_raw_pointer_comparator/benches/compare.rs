// Spike 013: head-to-head of the three comparators in `src/lib.rs`.
// `make bench` (or `cargo bench`) runs this.
use std::cmp::Ordering;
use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use raw_pointer_comparator_spike::{encode_int_record, regular, safe_fast, unsafe_trick};

/// Bucket-like values (#631's actual `bench_data.bucket` column: small,
/// mostly non-negative) — skews every comparison toward serial type 1
/// (1-byte integers), the realistic common case, not an adversarial mix
/// of widths.
fn bucket_like_values(n: usize, seed: u64) -> Vec<i64> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..n).map(|_| rng.gen_range(0..1000)).collect()
}

fn bench_pairwise(c: &mut Criterion) {
    let values = bucket_like_values(2, 1);
    let a = encode_int_record(values[0]);
    let b = encode_int_record(values[1]);

    let mut group = c.benchmark_group("compare_one_pair");
    group.bench_function("regular", |bencher| {
        bencher.iter(|| black_box(regular::compare(black_box(&a), black_box(&b))));
    });
    group.bench_function("safe_fast", |bencher| {
        bencher.iter(|| black_box(safe_fast::compare(black_box(&a), black_box(&b))));
    });
    group.bench_function("unsafe_trick", |bencher| {
        bencher.iter(|| black_box(unsafe { unsafe_trick::compare(black_box(&a), black_box(&b)) }));
    });
    group.finish();
}

/// Sorts a shuffled 50,000-row set of encoded records — the actual
/// usage shape (`SorterSort`'s `Vec::sort_by`), not just an isolated
/// comparison, so criterion sees the same call-count/branch-predictor
/// behavior a real sort would.
fn bench_sort(c: &mut Criterion) {
    const ROWS: usize = 50_000;
    let values = bucket_like_values(ROWS, 42);
    let records: Vec<Vec<u8>> = values.iter().map(|&v| encode_int_record(v)).collect();

    let mut group = c.benchmark_group("sort_50000_rows");
    group.bench_function("regular", |bencher| {
        bencher.iter_batched(
            || records.clone(),
            |mut recs| {
                recs.sort_by(|a, b| regular::compare(a, b));
                black_box(recs)
            },
            BatchSize::LargeInput,
        );
    });
    group.bench_function("safe_fast", |bencher| {
        bencher.iter_batched(
            || records.clone(),
            |mut recs| {
                recs.sort_by(|a, b| safe_fast::compare(a, b));
                black_box(recs)
            },
            BatchSize::LargeInput,
        );
    });
    group.bench_function("unsafe_trick", |bencher| {
        bencher.iter_batched(
            || records.clone(),
            |mut recs| {
                recs.sort_by(|a, b| unsafe { unsafe_trick::compare(a, b) });
                black_box(recs)
            },
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

/// Sanity check the three comparators actually agree before trusting
/// any timing difference between them — a bug making one "faster" by
/// comparing wrong would be worse than useless here.
fn assert_agreement() {
    let values = bucket_like_values(200, 7);
    let records: Vec<Vec<u8>> = values.iter().map(|&v| encode_int_record(v)).collect();
    for i in 0..records.len() {
        for j in 0..records.len() {
            let expected: Ordering = values[i].cmp(&values[j]);
            assert_eq!(regular::compare(&records[i], &records[j]), expected);
            assert_eq!(safe_fast::compare(&records[i], &records[j]), expected);
            assert_eq!(
                unsafe { unsafe_trick::compare(&records[i], &records[j]) },
                expected
            );
        }
    }
}

fn bench_all(c: &mut Criterion) {
    assert_agreement();
    bench_pairwise(c);
    bench_sort(c);
}

criterion_group!(benches, bench_all);
criterion_main!(benches);
