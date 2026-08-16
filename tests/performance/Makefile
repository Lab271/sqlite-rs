# Run individual performance tests/benches from this directory, without
# reaching for the full `make bench`/`make test` sweep in the repo root
# Makefile. Each target is scoped to exactly one file here so you can
# iterate on one scenario at a time.
#
# Usage: cd tests/performance && make point-lookup
# (or `make -C tests/performance point-lookup` from the repo root).

.PHONY: help point-lookup engine engine-point-lookup engine-full-scan engine-filter-scan engine-order-by-limit engine-expr-heavy engine-prepare-only all

help: ## List targets in this Makefile
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-24s\033[0m %s\n", $$1, $$2}'

point-lookup: ## Quick wall-clock demo: SeekRowid vs full scan (#137, no criterion/oracle needed)
	cargo test --locked --manifest-path ../../Cargo.toml --test point_lookup_perf -- --nocapture

# The criterion bench (engine.rs) needs the pinned-oracle-linked rusqlite
# build and pre-generated fixtures — see tools/bench_env.sh and
# `make fixtures-bench` in the repo root Makefile. These targets assume
# both are already set up (run `make -C ../.. fixtures-bench` once first)
# and just filter criterion's benchmark-name argument down to one
# scenario from tests/performance/engine.rs's SCENARIOS list.
engine: ## Run every engine.rs scenario (sqlite-rs vs rusqlite/oracle, all fixture sizes)
	bash -c 'cd ../.. && . ./tools/bench_env.sh && cargo bench --bench engine'

engine-point-lookup: ## Just the point_lookup scenario (WHERE id = <const>)
	bash -c 'cd ../.. && . ./tools/bench_env.sh && cargo bench --bench engine -- point_lookup'

engine-full-scan: ## Just the full_scan scenario
	bash -c 'cd ../.. && . ./tools/bench_env.sh && cargo bench --bench engine -- full_scan'

engine-filter-scan: ## Just the filter_scan scenario (WHERE x > 50000)
	bash -c 'cd ../.. && . ./tools/bench_env.sh && cargo bench --bench engine -- filter_scan'

engine-order-by-limit: ## Just the order_by_limit scenario
	bash -c 'cd ../.. && . ./tools/bench_env.sh && cargo bench --bench engine -- order_by_limit'

engine-expr-heavy: ## Just the expr_heavy scenario
	bash -c 'cd ../.. && . ./tools/bench_env.sh && cargo bench --bench engine -- expr_heavy'

engine-prepare-only: ## Just the prepare_only scenario (parse+codegen only, no execution)
	bash -c 'cd ../.. && . ./tools/bench_env.sh && cargo bench --bench engine -- prepare_only'

all: point-lookup engine ## Run the quick demo, then the full criterion bench
