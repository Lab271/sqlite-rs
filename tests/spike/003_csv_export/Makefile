# CSV-export spike (issue #12) — single throwaway experiment, no test
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

run: ## Export every user table of fixture.db to CSV
	cargo run --quiet -- fixture.db

fixture: ## Regenerate fixture.db via sqlite3 (requires sqlite3 on PATH)
	./gen_fixture.sh
