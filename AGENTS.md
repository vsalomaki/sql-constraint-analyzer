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

Run `make lint` and `make test` before calling a change done.

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
- Prove a fix by reverting it and watching a test fail.

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
