# Spike 012: Hash Aggregation Investigation
# Usage: make bench | make profile

BENCH_ENGINE = cargo bench --bench engine

.PHONY: bench profile clean

# Run the group_by_agg benchmark (sqlite-rs vs oracle)
bench:
	@bash -c '. ../../../tools/bench_env.sh && $(BENCH_ENGINE) -- group_by_agg'

# Sample the sqlite-rs side with macOS `sample` (no sudo required, unlike
# cargo-flamegraph's dtrace backend). Writes profile-group_by_agg-50mb.txt.
profile:
	@bash -c '. ../../../tools/bench_env.sh && \
		cargo bench --bench engine --no-run && \
		BIN=$$(ls -t ../../../target/release/deps/engine-* | grep -v "\." | head -1) && \
		$$BIN --bench "group_by_agg/bench_50mb.db/ours" --profile-time 15 & \
		sleep 3 && \
		PID=$$(ps -axo pid,comm | grep "$$(basename $$BIN)$$" | awk "{print \$$1}" | head -1) && \
		sample $$PID 8 -f profile-group_by_agg-50mb.txt && \
		wait'

clean:
	rm -f profile-group_by_agg-50mb.txt
