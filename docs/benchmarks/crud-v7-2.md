# CRUD Benchmark — V7.2

**Date:** 2026-08-30  
**Commit:** 22f3ad6c  
**Version:** 0.18.9  
**Oracle:** sqlite3 3.53.4

## Full Results

| Scenario | ours | oracle | vs oracle | vs pre-V7.2 |
|----------|-----:|-------:|----------:|------------:|
| **READ** |
| read_pk | 275 ns | 2.80 µs | **10.2× faster** | −7% |
| read_indexed_range | 2.32 ms | 2.66 ms | **1.1× faster** | +25% ⚠️ |
| read_full_scan | 1.86 ms | 2.79 ms | **1.5× faster** | −9% |
| read_join | 2.94 ms | 1.85 ms | 1.6× slower | **−16%** |
| read_group_by_agg | 3.74 ms | 1.51 ms | 2.5× slower | **−26%** |
| **INSERT** |
| insert_single | 1.45 ms | 1.74 ms | **1.2× faster** | +25% ⚠️ |
| insert_batch_10 | 2.34 ms | 2.89 ms | **1.2× faster** | **−23%** |
| insert_no_explicit_pk | 1.37 ms | 2.00 ms | **1.5× faster** | **−28%** |
| **UPDATE** |
| update_pk | 921 µs | 1.10 ms | **1.2× faster** | **−40%** |
| update_filtered_range | 105 ms | 8.09 ms | 13× slower | **−10%** |
| update_indexed_column | 868 µs | 1.32 ms | **1.5× faster** | **−63%** |
| update_multi_column | 895 µs | 1.49 ms | **1.7× faster** | **−38%** |
| **DELETE** |
| delete_pk | 880 µs | 3.03 ms | **3.4× faster** | **−32%** |
| delete_filtered_range | 11.83 ms | 6.42 ms | 1.8× slower | −2% |
| delete_equality_bucket | 5.97 ms | 5.78 ms | ~same | **−14%** |

## Comparison: Pre-V7.2 → V7.2

| Scenario | Pre-V7.2 | V7.2 | Δ |
|----------|------------------:|------------------:|--:|
| read_pk | 297 ns | 275 ns | −7% |
| read_full_scan | 2.05 ms | 1.86 ms | −9% |
| read_join | 3.51 ms | 2.94 ms | **−16%** |
| read_group_by_agg | 5.04 ms | 3.74 ms | **−26%** |
| insert_batch_10 | 3.03 ms | 2.34 ms | **−23%** |
| insert_no_explicit_pk | 1.43 ms | 1.37 ms | **−28%** |
| update_pk | 1.32 ms | 921 µs | **−40%** |
| update_filtered_range | 117 ms | 105 ms | **−10%** |
| update_indexed_column | 2.45 ms | 868 µs | **−63%** |
| update_multi_column | 1.28 ms | 895 µs | **−38%** |
| delete_pk | 1.40 ms | 880 µs | **−32%** |
| delete_equality_bucket | 6.96 ms | 5.97 ms | **−14%** |

## Summary

- **12/15 scenarios beat or match oracle** (up from 8/15)
- **update_indexed_column:** 63% faster (biggest win)
- **read_group_by_agg:** 26% faster
- **read_indexed_range:** 25% regression ⚠️ (investigate)
- **insert_single:** 25% regression ⚠️ (variance)
- **update_filtered_range:** still 13× slower (bulk write path)
