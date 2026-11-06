.PHONY: lint fmt test check-all install-hooks fixtures oracle-check oracle-stop sqlcl-fixtures perf

GEN = cargo run -q -F gen --bin sqlca-gen -- --dialect oracle
FIXTURES = tests/fixtures/gen/oracle

## Loads generated exports and their sqlca fix output into Oracle Free (Docker) and checks what Oracle rejects.
oracle-check:
	scripts/oracle/check.sh

## Removes the Oracle Free container that oracle-check keeps between runs.
oracle-stop:
	scripts/oracle/check.sh --stop

## Rewrites tests/fixtures/sqlcl/: a generated export loaded into Oracle Free and exported again with SQLcl.
sqlcl-fixtures:
	scripts/oracle/sqlcl-fixtures.sh

## Regenerates the committed generator fixtures; review the diff before committing it.
fixtures:
	rm -rf $(FIXTURES)
	$(GEN) --out $(FIXTURES)/separate-sqldev --rows 20
	$(GEN) --out $(FIXTURES)/single-all-crlf-1252 --rows 20 --layout single --noise all --line-endings crlf --encoding windows-1252
	$(GEN) --out $(FIXTURES)/type-after-data-shuffled --rows 20 --layout type --noise all --constraints after-data --shuffle-order

## Times sqlca check and fix on a ~1 GB and a ~2.4 GB generated export in gen-out/ and checks their output (Linux only).
perf:
	scripts/perf.sh

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
