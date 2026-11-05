.PHONY: lint fmt test check-all install-hooks fixtures oracle-check oracle-stop

GEN = cargo run -q -F gen --bin sqlca-gen -- --dialect oracle
FIXTURES = tests/fixtures/gen/oracle

## Loads generated exports into Oracle Free (Docker) and checks Oracle rejects what the manifests expect.
oracle-check:
	scripts/oracle/check.sh

## Removes the Oracle Free container that oracle-check keeps between runs.
oracle-stop:
	scripts/oracle/check.sh --stop

## Regenerates the committed generator fixtures; review the diff before committing it.
fixtures:
	rm -rf $(FIXTURES)
	$(GEN) --out $(FIXTURES)/separate-sqldev --rows 20
	$(GEN) --out $(FIXTURES)/single-all-crlf-1252 --rows 20 --layout single --noise all --line-endings crlf --encoding windows-1252
	$(GEN) --out $(FIXTURES)/type-after-data-shuffled --rows 20 --layout type --noise all --constraints after-data --shuffle-order

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
