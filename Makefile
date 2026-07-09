.PHONY: help setup check release

help: ## Show available targets
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-10s\033[0m %s\n", $$1, $$2}'

setup: ## Point git at the tracked hooks (run once after cloning)
	git config core.hooksPath .githooks

check: ## Run the full local quality gate: fmt, clippy, tests
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

release: ## Roll [Unreleased], tag vX.Y.Z, push. Crate publish happens in CI. Usage: make release [bump=minor|major]
	@bin/release.sh "$(bump)"
