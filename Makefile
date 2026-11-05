.PHONY: lint fmt test check-all install-hooks

## Turns on the gitleaks pre-push hook for this clone.
install-hooks:
	git config core.hooksPath .githooks

## Format check, dependency policy, then clippy with warnings as errors.
lint:
	cargo fmt --check
	cargo deny check
	cargo clippy --all-targets --all-features -- -D warnings

fmt:
	cargo fmt

test:
	cargo nextest run --all-features

check-all: lint test
