.DEFAULT_GOAL := help

.PHONY: help build format lint deps test check hooks tools

help: ## List available targets with their descriptions
	@grep -hE '^[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*## "} {printf "  \033[36m%-8s\033[0m %s\n", $$1, $$2}'

build: ## Build the release binary
	cargo build --release --locked

format: ## Format the code (rustfmt)
	cargo fmt --all

lint: ## Lint every crate and target, warnings as errors (clippy)
	cargo clippy --workspace --all-targets --locked -- -D warnings

deps: ## Unused dependencies (cargo-machete), advisories, licenses and sources (cargo-deny)
	cargo machete
	cargo deny --locked check

test: ## Run the test suite, the determinism check among them
	cargo test --workspace --locked

check: ## Everything CI runs, but the git hooks: formatting, lints, dependencies and tests
	cargo fmt --all --check
	$(MAKE) lint deps test

hooks: ## Install the git hooks, then run them over the whole tree (prek)
	prek install
	prek run --all-files

tools: ## Install the cargo tools `deps` runs
	cargo install --locked cargo-deny@0.20.2 cargo-machete@0.9.2
