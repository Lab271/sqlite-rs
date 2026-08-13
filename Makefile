# sqlite-rs

.DEFAULT_GOAL := help

.PHONY: help test-spikes

help: ## Show this help
	@echo ""
	@awk 'BEGIN {FS = ":.*?## "} \
	  /^# === .* ===$$/  { sub(/^# === /, ""); sub(/ ===$$/, ""); printf "\n\033[33m%s\033[0m\n", $$0 } \
	  /^[a-zA-Z0-9_-]+:.*?## / { printf "  \033[36m%-24s\033[0m %s\n", $$1, $$2 }' \
	  $(MAKEFILE_LIST)
	@echo ""

# === Spikes ===

test-spikes: ## Run all parser-spike variants (tests/spike/001_parser)
	$(MAKE) -C tests/spike/001_parser test
