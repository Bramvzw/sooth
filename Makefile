.PHONY: help setup check smoke release

help: ## Show available targets
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-10s\033[0m %s\n", $$1, $$2}'

setup: ## Point git at the tracked hooks (run once after cloning)
	git config core.hooksPath .githooks

check: ## Run the full local quality gate: fmt, clippy, tests
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

smoke: ## Drive sooth against real runners (needs composer, npm; pytest if installed). Usage: make smoke [runners="phpunit jest"]
	cargo build
	@for runner in $(or $(runners),phpunit pest composer php jest pytest); do \
		if [ "$$runner" = pytest ] && ! command -v pytest >/dev/null; then \
			echo "── pytest: skipped (not installed)"; continue; \
		fi; \
		tests/smoke/run.sh $$runner || exit 1; \
	done

release: ## Roll [Unreleased], tag vX.Y.Z, push. Crate publish happens in CI. Usage: make release [bump=minor|major]
	@bin/release.sh "$(bump)"
