# Spike 014: embedding-API kernel — Send+Sync handle over a streaming VDBE
# Usage: make test | make bench
#
# `test` gates `bench`: the three prototypes must agree on every row
# before any timing is trusted (spike 013's precedent).

.PHONY: test bench clean

# Correctness first: agreement across batch/worker_batch/worker_stream,
# the compile-time Send+Sync assertion, and drop-mid-iteration.
test:
	cargo test

# Per-row latency, total throughput, and peak-heap for a large result.
bench:
	cargo bench

clean:
	cargo clean
	rm -f *.db
