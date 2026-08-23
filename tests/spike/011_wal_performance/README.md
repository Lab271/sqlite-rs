# Spike 011: WAL Performance Regression

**Issue:** #438 (investigation), #437 (bug)

## Problem

WAL mode is 27% slower than DELETE journal mode — opposite of expected.

## Baseline Results (2026-08-23)

| Mode | sqlite-rs | Oracle | Ratio |
|------|----------:|-------:|------:|
| DELETE journal | 55.1 ms | 3.2 ms | 17.2× |
| WAL | 69.8 ms | 3.3 ms | 21.2× |

**Delta:** WAL is 14.7 ms slower per batch (27% regression)

### Other V6 benchmarks

| Scenario | sqlite-rs | Notes |
|----------|----------:|-------|
| concurrent_read_write (20 scans) | 564 ms | WAL enables this |
| checkpoint_passive | 29.7 ms | |
| CTE vs inline | 30.0 ms vs 30.0 ms | No CTE optimization |

## Hypotheses

1. **WAL index rescan** — ADR-0026 mentions rescanning on commit
2. **SHM lock overhead** — too many lock acquisitions
3. **Checkpoint interference** — passive checkpoint during benchmark
4. **Frame append inefficiency** — not batching writes

## Investigation

```bash
make profile-journal
make profile-wal
make compare
```

## Findings

(To be filled during investigation)

## Recommendation

(To be filled after investigation)
