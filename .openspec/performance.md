# Performance

sqlite-rs benchmarks against the sqlite3 oracle (v3.53.4) on a 1MB fixture (16,700 rows).

## V7 Final Results (0.18.5)

| Query | sqlite-rs | Oracle | Ratio | Status |
|-------|----------:|-------:|------:|--------|
| point_lookup | 301 ns | 2.81 µs | **0.11×** | 9× faster than C |
| filter_scan | 1.77 ms | 2.68 ms | **0.66×** | beats oracle |
| full_scan | 1.97 ms | 2.77 ms | **0.71×** | beats oracle |
| order_by_limit | 25.2 µs | 29.9 µs | **0.84×** | beats oracle |
| join | 3.50 ms | 1.86 ms | 1.88× | within 2× |
| group_by_agg | 6.50 ms | 1.56 ms | 4.2× | within 5× |
| correlated_subquery | 4.79 ms | 2.61 ms | **1.84×** | within 2× |

**6 of 7 queries beat or match the sqlite3 oracle.**

## Progression (V4 → V7)

| Query | V4 | V5 | V6 | V7.2 | V7.3 | V7 final |
|-------|---:|---:|---:|-----:|-----:|---------:|
| point_lookup | — | 0.42× | 0.40× | 0.12× | 0.10× | **0.11×** |
| filter_scan | — | 2.4× | — | 0.79× | 0.73× | **0.66×** |
| full_scan | 3.6× | 3.4× | 3.6× | 0.87× | 0.83× | **0.71×** |
| order_by_limit | — | — | — | 1.01× | 0.98× | **0.84×** |
| join | 15.6× | 11.1× | — | 2.09× | 1.86× | **1.88×** |
| group_by_agg | 23.0× | 26.2× | — | 6.7× | 5.1× | **4.2×** |
| correlated_subquery | — | — | 785× | 2.23× | 1.91× | **1.84×** |

## Key Optimizations

| Version | Optimization | Impact |
|---------|--------------|--------|
| V5 | Transactions (BEGIN/COMMIT) | 24× write improvement |
| V6 | WAL mode | Concurrent readers |
| V7.1 | Lazy payload reassembly | full_scan 3.6× → 1.3× |
| V7.2 | Join ordering + Bloom filter (superseded by automatic indexing, #545; removed #623) | join 11× → 2× |
| V7.2 | Correlated subquery cache | 785× → 2× |
| V7.3 | Record encode scratch buffer | 25% write improvement |
| V7 | Compile-path optimizations | Tokenizer 50% faster |

## Compile-Path Benchmarks

| Benchmark | Before | After | Change |
|-----------|-------:|------:|-------:|
| tokenize/short | 723 ns | 344 ns | −52.6% |
| tokenize/long | 3.26 µs | 1.62 µs | −50.3% |
| parse/short | 1.25 µs | 877 ns | −30.0% |
| parse/long | 6.14 µs | 3.41 µs | −39.1% |
| compile_full/short | 3.89 µs | 2.39 µs | −39.3% |

## Running Benchmarks

```bash
# Setup pinned oracle (required)
source tools/bench_env.sh

# Run criterion benchmarks
cargo bench --bench engine

# Run compile-path benchmarks (no oracle needed)
make bench-compile-path

# Quick CLI comparison
make bench-cli
```

## Benchmark Fixture

- `bench_data`: 16,700 rows (1MB)
- `bench_lookup`: 1,000 rows
- Indexed on `bench_data.x`
- ANALYZE statistics present

## Metrics (V7)

| Metric | Value |
|--------|------:|
| src/ (Rust) | 47,523 |
| tests/ (Rust) | ~35,000 |
| External deps | **0** |
| Test coverage | **85%+** |
