# Spike 010: WAL Performance Investigation
# Usage: make bench | make profile-journal | make profile-wal | make compare

BENCH_V6 = cargo bench --bench v6
FLAMEGRAPH = cargo flamegraph --bench v6

.PHONY: bench profile-journal profile-wal compare clean

# Run both journal and WAL benchmarks
bench:
	@echo "=== DELETE Journal Mode ==="
	$(BENCH_V6) -- "insert_batch_wal_journal"
	@echo ""
	@echo "=== WAL Mode ==="
	$(BENCH_V6) -- "insert_batch_wal_wal"

# Profile DELETE journal mode
profile-journal:
	$(FLAMEGRAPH) -o flamegraph-journal.svg -- --bench "insert_batch_wal_journal"
	@echo "Output: flamegraph-journal.svg"

# Profile WAL mode
profile-wal:
	$(FLAMEGRAPH) -o flamegraph-wal.svg -- --bench "insert_batch_wal_wal"
	@echo "Output: flamegraph-wal.svg"

# Run both profiles
compare: profile-journal profile-wal
	@echo ""
	@echo "Compare flamegraph-journal.svg vs flamegraph-wal.svg"
	@echo "Look for hot spots unique to WAL path"

# Quick benchmark (fewer samples)
quick:
	$(BENCH_V6) -- --quick "insert_batch_wal"

clean:
	rm -f flamegraph-*.svg perf.data perf.data.old
