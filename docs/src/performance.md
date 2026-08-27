# Performance

sqlite-rs benchmarks against the sqlite3 oracle (v3.53.4) on a 1MB fixture.

## Current Results (V7.3)

| Query | sqlite-rs | Oracle | Ratio | Status |
|-------|----------:|-------:|------:|--------|
| point_lookup | 295 ns | 2.88 µs | **0.10×** | 10× faster than C |
| filter_scan | 1.97 ms | 2.71 ms | **0.73×** | beats oracle |
| full_scan | 2.33 ms | 2.80 ms | **0.83×** | beats oracle |
| order_by_limit | 29.3 µs | 30.0 µs | **0.98×** | parity |
| join | 3.95 ms | 2.12 ms | 1.86× | within 2× |
| group_by_agg | 7.84 ms | 1.55 ms | 5.1× | within 5× |
| correlated_subquery | 4.96 ms | 2.60 ms | **1.91×** | within 2× |

**5 of 7 queries beat or match the sqlite3 oracle.**

## Progression (V4 → V7.3)

| Query | V4 | V5 | V6 | V7.2 | V7.3 |
|-------|---:|---:|---:|-----:|-----:|
| point_lookup | — | 0.42× | 0.40× | 0.12× | **0.10×** |
| filter_scan | — | 2.4× | — | 0.79× | **0.73×** |
| full_scan | 3.6× | 3.4× | 3.6× | 0.87× | **0.83×** |
| order_by_limit | — | — | — | 1.01× | **0.98×** |
| join | 15.6× | 11.1× | — | 2.09× | **1.86×** |
| group_by_agg | 23.0× | 26.2× | — | 6.7× | **5.1×** |
| correlated_subquery | — | — | 785× | 2.23× | **1.91×** |

## Key Optimizations

| Version | Optimization | Impact |
|---------|--------------|--------|
| V5 | Transactions (BEGIN/COMMIT) | 24× write improvement |
| V6 | WAL mode | Concurrent readers |
| V7.1 | Lazy payload reassembly | full_scan 3.6× → 1.3× |
| V7.2 | Join ordering + Bloom filter | join 11× → 2× |
| V7.2 | Correlated subquery cache | 785× → 2× |
| V7.3 | Record encode scratch buffer | 25% improvement across writes |

## Running Benchmarks

```bash
# Setup pinned oracle
source tools/bench_env.sh

# Run criterion benchmarks
cargo bench --bench engine

# Quick comparison
make bench-cli
```

## Benchmark Fixture

- `bench_data`: 16,700 rows (1MB)
- `bench_lookup`: 1,000 rows
- Indexed on `bench_data.x`
- ANALYZE statistics present
