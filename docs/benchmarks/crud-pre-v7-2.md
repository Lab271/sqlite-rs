# CRUD Benchmark — Pre V7-2

**Date:** 2026-08-30  
**Commit:** 24fd5275 (post PRAGMA synchronous + fsync ADRs)  
**Oracle:** sqlite3 3.53.4  
**Version:** 0.18.8

## Full Results

| Scenario | ours | oracle | vs oracle | vs prev |
|----------|-----:|-------:|----------:|--------:|
| **READ** |
| read_pk | 297 ns | 2.87 µs | **9.7× faster** | +6% |
| read_indexed_range | 1.85 ms | 2.65 ms | **1.4× faster** | +9% |
| read_full_scan | 2.05 ms | 2.81 ms | **1.4× faster** | +4% |
| read_join | 3.51 ms | 1.85 ms | 1.9× slower | +2% |
| read_group_by_agg | 5.04 ms | 1.54 ms | 3.3× slower | +1% |
| **INSERT** |
| insert_single | 1.21 ms | 1.69 ms | **1.4× faster** | **−92%** |
| insert_batch_10 | 3.03 ms | 1.78 ms | 1.7× slower | **−82%** |
| insert_no_explicit_pk | 1.43 ms | 3.30 ms | **2.3× faster** | **−86%** |
| **UPDATE** |
| update_pk | 1.32 ms | 1.49 ms | **1.1× faster** | **−89%** |
| update_filtered_range | 117 ms | 7.57 ms | 15× slower | **−83%** |
| update_indexed_column | 2.45 ms | 2.27 ms | 1.1× slower | **−81%** |
| update_multi_column | 1.28 ms | 2.24 ms | **1.8× faster** | **−88%** |
| **DELETE** |
| delete_pk | 1.40 ms | 3.05 ms | **2.2× faster** | **−88%** |
| delete_filtered_range | 12.10 ms | 6.16 ms | 2.0× slower | **−79%** |
| delete_equality_bucket | 6.96 ms | 7.03 ms | ~same | **−60%** |

## Progression (Last 3 Weeks)

| Scenario | Aug 21 (baseline) | Aug 30 (pre V7-2) | Δ |
|----------|------------------:|------------------:|--:|
| insert_single | ~14.8 ms | 1.21 ms | **−92%** |
| insert_batch_10 | ~16.6 ms | 3.03 ms | **−82%** |
| insert_no_explicit_pk | ~10.4 ms | 1.43 ms | **−86%** |
| update_pk | ~12.2 ms | 1.32 ms | **−89%** |
| update_filtered_range | ~670 ms | 117 ms | **−83%** |
| update_indexed_column | ~12.7 ms | 2.45 ms | **−81%** |
| update_multi_column | ~10.9 ms | 1.28 ms | **−88%** |
| delete_pk | ~12.0 ms | 1.40 ms | **−88%** |
| delete_filtered_range | ~58.3 ms | 12.10 ms | **−79%** |
| delete_equality_bucket | ~17.4 ms | 6.96 ms | **−60%** |

*Baseline estimated from % change in criterion output.*

## Key Optimizations (Aug 21 → Aug 30)

| PR | Optimization | Impact |
|----|--------------|--------|
| #640 | Cache WAL resume state on Pager | Skip per-flush rescan |
| #643 | Cache parsed CREATE TABLE constraints | Avoid DDL re-parse per write |
| #644 | Tokenizer borrows source string | Zero-copy tokenize |
| #648 | Binary-search index leaf position | Skip full-page decode |
| #652 | Plain fsync(2) on macOS | 10× faster than F_FULLFSYNC |

## Summary

- **10/15 scenarios improved** (all writes, reads unchanged)
- **9 scenarios 79–92% faster** than 9 days ago
- **8/15 scenarios now beat or match oracle**
- **update_filtered_range** still 15× slower (bulk write path)
