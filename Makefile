.PHONY: lint fmt test check-all

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
