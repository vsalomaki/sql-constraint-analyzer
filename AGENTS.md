# sql-constraint-analyzer

A Rust CLI (`sqlca`) that checks Oracle SQL exports for rows violating declared constraints and writes fixed copies,
plus a test-data generator (`sqlca-gen`). One crate: library `sqlca` (`src/lib.rs`), binaries `src/main.rs` and
`src/bin/sqlca-gen.rs`.

If `PLAN.md` exists in the checkout, it holds the design and milestones. It is local only and git-ignored: never
commit it, quote it in commit messages, or copy it into tracked files.

## Setup

Once per clone, `make install-hooks` turns on `.githooks/pre-push`, which scans the commits being pushed for secrets
with [gitleaks](https://github.com/gitleaks/gitleaks#installation) (needs it in `PATH`). Skip with
`git push --no-verify`, discouraged. A false positive gets the narrowest allowlist entry in `.gitleaks.toml`.

## Commands

| Command | Does |
|---|---|
| `make install-hooks` | `git config core.hooksPath .githooks` |
| `make lint` | `cargo fmt --check`, `cargo deny check`, clippy on all targets and features with `-D warnings` |
| `make test` | `cargo nextest run --all-features` |
| `make fmt` | `cargo fmt` |
| `bacon` | clippy on save; `t` for tests |
| `cargo run -- <args>` | Runs `sqlca` |
| `cargo run -F gen --bin sqlca-gen -- <args>` | Runs the generator; it builds only with the `gen` feature |
| `make fixtures` | Regenerates `tests/fixtures/gen/<dialect>/` from the generator; a test fails until fixtures match the generator, so review the diff and commit it with the generator change |
| `make oracle-check` | Starts Oracle Free in Docker (kept between runs) and checks that Oracle rejects exactly what the generator's manifests expect, and only what `sqlca fix` leaves in its output; see "Ground truth" below |
| `make oracle-stop` | Removes that container |
| `make sqlcl-fixtures` | Rewrites `tests/fixtures/sqlcl/` with Oracle Free and SQLcl in Docker |
| `make perf` | Generates a ~1 GB and a ~2.4 GB export into `gen-out/` (reused while their options match), times `sqlca check` (release) on both in both modes and `sqlca fix --out` on both (into a temporary folder in `gen-out/`), and fails unless the findings equal the manifest and fix removes exactly the rows the manifest implies, keeping every other row byte for byte and every other finding; prints wall time, MB/s, peak RSS (Linux only) and fix's time against the check's |

Run `make lint` and `make test` before calling a change done.

## Dialects

Oracle is the only dialect, but nothing outside `src/dialect/` may depend on one. Neutral code (input handling,
catalog, checks, reporting, fixing, the generator's model, rows and plants) never names a dialect.

- `src/dialect/mod.rs` holds `DialectId` and `Rules`: identifier folding and quoting, whether `''` is NULL, `CHAR`
  padding, how partially NULL unique keys compare, deferrable foreign keys. Semantics differ through `Rules`, never
  through `if oracle` in neutral code. Neutral code reaches a dialect only through `DialectId`'s methods
  (`pieces`, `parse`, `evaluate`, `script_includes`, `rules`), each a `match` on the dialect.
- `src/dialect/<name>/` holds the syntax. For Oracle: `split.rs` (script into pieces, on bytes, by SQL\*Plus or
  SQLcl rules: `Runner`, default SQL\*Plus), `lex.rs` (one statement into tokens), and `synth.rs` (the
  generator's DDL, literals, `INSERT` shapes, SQL\*Plus headers, `load.sql`, and how many statements each wrote)
  behind the `synth::Render` trait. `parse.rs` sends each piece to `insert.rs` (`INSERT` rows) or `ddl.rs`
  (`CREATE TABLE`, `ALTER TABLE`, `CREATE INDEX`), both over the token cursor in `cursor.rs`, or names it as
  skipped; `eval.rs` says what a value expression means. `src/source.rs` holds the neutral `Piece` and `Span` the
  splitter produces, `src/statement.rs` the statements, and `src/value.rs` canonical values.
- `src/catalog.rs` (neutral) builds tables, ordered columns and resolved constraints from the DDL statements. It
  applies every `CREATE TABLE` before the rest, since a version-sorted glob reads `ref_constraints/` before
  `tables/`.
- A planted violation that exists only under some rules (`''` in a NOT NULL column, a partially NULL unique
  duplicate, a deferred FK) is planted only when the dialect's `Rules` make it one.
- Adding a dialect means a new folder, a `DialectId` value, its `Rules` and `Render`, and its fixtures under
  `tests/fixtures/gen/<dialect>/`. Its ground-truth check goes in `scripts/<dialect>/` and
  `src/dialect/<dialect>/check.rs`.

## Conventions

- **Lint exceptions** are `#[expect(lint, reason = "…")]` on the narrowest item. `#[allow]` and reasonless
  exceptions fail the build.
- **Nothing panics or swallows a result unexplained.** Clippy fails on `unwrap`, `expect`, `panic!`,
  `unreachable!`, `todo!`, indexing and slicing, byte-slicing a `str`, `process::exit`, `let _ =` on a `#[must_use]`
  value and a discarded `.ok()`. Prefer the form that can't fail (`get`, `split_once`, iterators, `?`). `#[test]`
  functions may unwrap, expect, panic and index (`clippy.toml`); test helpers need an `#[expect]` with a reason.
- **Output:** findings and listings go to stdout, everything about the run (header, progress, warnings, errors) to
  stderr, both through `Write` handles: `print!`/`eprint!` fail the lint. `main` returns `ExitCode`.
- **Exit codes:** `0` clean, `1` violations found, `2` usage, I/O or fatal errors. clap's usage errors are already 2.
- **Byte offsets are `u64`.** Inputs run to several GB; a single file can pass 4 GiB.
- **Memory grows with key count, not input size.** Stream statements; never read a whole data file into memory.
- **Oracle semantics** (case folding, `''` is NULL, NULL-satisfied FKs, constraint states) are decided rules with
  tests. A change to one changes test expectations on purpose, never as a side effect.
- **Dependencies:** add with `cargo add`; pin features (`default-features = false` when the defaults pull in more
  than needed). A dependency that brings a new licence adds it to `deny.toml`'s allowlist, permissive licences only.
  The generator's dependencies are `optional = true` and enabled by the `gen` feature.

## Engineering principles

1. **Prefer removing over guarding.** When a fix seems to need another guard, look first for something to delete.
   - A guard is code, and code fails. Guards added to satisfy a review finding are a common source of the next one.
   - Ask what would make the failure impossible. Making a component's identity explicit replaces the stale state it
     would otherwise defend against, and deletes the defending code with it.
   - A reviewer asking for more logic is the moment to step back. Propose the removal instead, and say why it closes
     the case by construction.
   - Example: a row `fix` removes never puts its key into the parent map, so its children fail in the same pass;
     there is no separate sweep for orphans to keep in sync.

2. **Avoid locks for concurrency.** Mutexes, shared caches and claim tokens usually create more issues than they
   resolve.
   - Each needs its own answers: a holder that panics, a claim that outlives its work, a release by the wrong owner.
     Every answer is more code, and more places for the next review to find a hole.
   - A race usually means two paths do the same job. Let only one of them do it, and the race is gone by
     construction.
   - Example: parallel parsing, when it comes, gives each file its own key maps and merges them after; there is no
     shared map behind a lock.

3. **Avoid cumbersome safety checks.** A pile of checks around one operation usually means the approach is wrong.
   - First ask whether the problem is approached correctly.
   - Don't verify that a platform primitive did what it returned: a write, a delete, a file move.
   - Check the failure a dependency is known to swallow, not what a future version might do.
   - Undo only what you changed. Leftovers nothing references are not worth cleanup code.
   - When successive review rounds each add a check to the same function, stop answering findings and reconsider
     the design.
   - Example: an in-place fix writes a temporary file beside the original and renames it over; nothing re-reads the
     file to confirm the rename.

4. **Cover the failure modes that can really happen.** Handle the inputs and failures the code will actually meet:
   malformed SQL, a statement cut off at end of file, an unknown table, a file that can't be read. For the very
   unlikely ones, apply 1–3.

5. **Engineered enough.** Match the complexity to the problem: fragile hacks and premature abstraction both cost.

6. **Explicit over clever.** Prefer readable, obvious code to compact code.

7. **Weigh review findings against the complexity they add.** Reviewers, human or model, optimise for correctness
   and will always find something to add. Treat a finding as a claim and verify it against the code. When a fix is
   right, prove it: revert it and confirm a test fails.

## Tests

- Unit tests sit next to the code. Integration tests are modules of one binary, `tests/it/main.rs`: a new area adds
  a module there, not a file in `tests/`.
- Fixtures under `tests/fixtures/` are byte-exact exports (CRLF, trailing spaces, other encodings);
  `.gitattributes` and `.editorconfig` leave their bytes alone.
- **Only synthetic data is committed:** `sqlca-gen` output or Oracle exports of synthetic data. A real export is run
  locally and never committed; what it teaches becomes a generator option and a synthetic fixture.
- Large generated exports (GB scale) go to `gen-out/`, which is git-ignored.
- `tests/fixtures/sqlcl/` (`make sqlcl-fixtures`, needs Docker) is a generated export loaded into Oracle Free and
  exported again by SQLcl through `DBMS_METADATA`, in SQL Developer's separate-files layout with `order.txt` and
  `counts.csv`. It has no violations. Its `DBMS_METADATA` DDL ends each table with an `INSERT … SELECT … FROM …@ KU$`
  statement, as this Oracle version writes it; the analyzer skips it as an `INSERT … SELECT`. Rows that got
  `SYSTIMESTAMP` change on every regeneration, so review the diff.
- Prove a fix by reverting it and watching a test fail.

### Ground truth

`make oracle-check` (`scripts/oracle/check.sh`, about a minute) runs the ignored test
`oracle_agrees_with_the_manifest_and_the_fix` against Oracle Free in Docker: image pinned by digest, no published
port, a throwaway password. It loads generated exports and requires Oracle's rejections to match the manifest, by
file and line where SQL\*Plus allows. Then it fixes the same exports (default policy, with `--annotate`, and
`--on-duplicate keep-last --annotate`, `--on-fk null --annotate`; `--load-order` when constraints come before the data), requires each output
with its removed rows put back to be its input byte for byte, loads it into fresh schemas, and requires that Oracle
rejects no duplicate, NULL or missing-parent row other than a child the fix leaves before its parent, that its
rejections match the fix's check of its output, and that it holds the output's rows less those it rejected. Run it
after any change to the generator's schema, plants or Oracle syntax, or to what `fix` writes. A failure is a
generator bug, a fix bug or a wrong Oracle rule: find out which before changing any. A new `ORA-` code the test
doesn't map means SQL Oracle doesn't accept.

## CI

`.github/workflows/ci.yml` runs on pushes to `main` and on pull requests: `lint` (rustfmt, cargo-deny, clippy) on
Linux, and `test` (a default-features build, then nextest with all features) on Linux, Windows and macOS.

- **Pull requests from forks never run** any workflow: every job is gated on the head branch being in this
  repository. Keep that condition on every job of every workflow, and never use `pull_request_target`.
- **Actions are pinned by commit SHA** with the version in a comment; Dependabot (`.github/dependabot.yml`) proposes
  updates weekly, after a 7-day cooldown. Permissions default to `contents: read`; a job that needs more asks for it
  itself.
- Tool versions (`cargo-deny`, `cargo-nextest`) are pinned in the workflow. Bumping one means bumping it there and
  checking the local version.
- Check workflow edits with `docker run --rm -v "$PWD:/repo" -w /repo rhysd/actionlint:1.7.7`.
- `.github/workflows/oracle.yml` runs `scripts/oracle/check.sh` on a runner, started by hand only (`workflow_dispatch`).
- `.github/workflows/perf.yml` runs `make perf` the same way, by hand only. Its numbers are a trend, not a gate.
- `.github/workflows/release.yml` runs on a `v*` tag: it fails unless the tag is `v` + Cargo.toml's `version`,
  builds `sqlca` with `--profile dist` (Linux x86_64 and aarch64 as gnu on Ubuntu 22.04 for speed and as static
  musl for reach, macOS aarch64, Windows x86_64),
  and creates a draft release with the archives, `SHA256SUMS` and a build-provenance attestation. A manual run
  only builds the archives, as workflow artifacts. `[profile.release]` is the perf baseline; shipped binaries use
  `[profile.dist]`.
- The repository setting "Require approval for all external contributors" holds a fork's run for approval as a
  second layer.

### Releasing

1. Bump `version` in `Cargo.toml` (and `Cargo.lock`, which `cargo build` updates) in a pull request; merge it.
2. Tag the merged commit on `main` and push the tag: `git tag vX.Y.Z origin/main && git push origin vX.Y.Z`.
3. Review the draft release `release.yml` creates (archives, notes) and publish it by hand.

The "release tags" ruleset protects `v*` tags: they can't be moved or deleted, so a wrong tag means a new version,
not a re-tag.

A dispatch of `release.yml` (`gh workflow run release.yml --ref <branch>`) is a dry run of the build.

## Git workflow

- **Never push to `main`.** Every change goes through a branch and a pull request. The `main` ruleset enforces it:
  no direct or force pushes, linear history, review threads resolved, and the required checks (`lint`,
  `test (ubuntu-24.04)`, `test (windows-2025)`, `test (macos-15)`) green on a branch that is up to date with `main`.
  No approval is needed. The ruleset is live GitHub state; check it with
  `gh api repos/vsalomaki/sql-constraint-analyzer/rulesets` when GitHub behaves differently from this. Renaming a CI
  job means updating the ruleset's required checks in the same change.
- **Branches:** `<type>/<issue>-<slug>` with an issue, `<type>/<slug>` without (`feat/test-data-generator`). Types:
  `feat`, `fix`, `chore`, `refactor`, `docs`, `test`, `ci`.
- **Commits:** Conventional Commits, `<type>(#<issue>): <description>` with an issue, `<type>(<area>): <description>`
  without.
- **Rebase, never merge:** rebase on `origin/main`. Review fixes amend the commit they belong to and force-push the
  branch, rather than adding fixup commits.
- **Merging:** rebase only (`gh pr merge <n> --rebase`); squash and merge commits are off, and GitHub deletes the
  branch. Rebase-merge replays every commit, so squash locally first unless each commit is meant to land.
- **PR body:** `## Summary` (what and why) and `## Test plan` (what was run, what a reviewer can rerun).
- **After a merge:** remove the PR's worktree and delete its local branch with `git branch -D` (rebase-merge rewrites
  the commits, so `-d` refuses).

## External review

Non-trivial PRs get a Codex review before merging:

```sh
CX=$(ls -d ~/.claude/plugins/cache/openai-codex/codex/*/scripts/codex-companion.mjs | sort -V | tail -1)
node "$CX" adversarial-review --background --base main --scope branch "<focus text>"
node "$CX" status --all      # match the job id you launched
node "$CX" result <job-id>
```

- Run the launch command in the background; it can block past a tool timeout. A job takes a few minutes. One that
  answers within seconds with a generic reply did no work: relaunch it.
- A finding is a claim: verify it against the code, and reject it with reasoning when it doesn't hold. Weigh the
  complexity a fix adds (see "Engineering principles"); prove a fix by reverting it and watching a test fail.
- When several reviewers run, wait for all of them, then make one round of changes. Say in the PR what the review
  found and what changed.
