# WAL frame-reading spike (issue #7) — single throwaway experiment, no test
# harness by design; verification is diffing `run`'s output against the
# sqlite3 oracle by hand (see findings.md).

.DEFAULT_GOAL := help

.PHONY: help run fixture

help: ## Show this help
	@echo ""
	@awk 'BEGIN {FS = ":.*?## "} \
	  /^# === .* ===$$/  { sub(/^# === /, ""); sub(/ ===$$/, ""); printf "\n\033[33m%s\033[0m\n", $$0 } \
	  /^[a-zA-Z0-9_-]+:.*?## / { printf "  \033[36m%-24s\033[0m %s\n", $$1, $$2 }' \
	  $(MAKEFILE_LIST)
	@echo ""

# === Spike ===

run: ## Parse fixture.db + fixture.db-wal and print decoded rows
	cargo run --quiet

fixture: ## Regenerate fixture.db / fixture.db-wal via sqlite3 (requires sqlite3 on PATH)
	./gen_fixture.sh
