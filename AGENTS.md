# sql-constraint-analyzer

A Rust CLI (`sqlca`) that checks Oracle SQL exports for rows violating declared constraints and writes fixed copies,
plus a test-data generator (`sqlca-gen`). One crate: library `sqlca` (`src/lib.rs`), binaries `src/main.rs` and
`src/bin/sqlca-gen.rs`.

If `PLAN.md` exists in the checkout, it holds the design and milestones. It is local only and git-ignored: never
commit it, quote it in commit messages, or copy it into tracked files.

## Commands

| Command | Does |
|---|---|
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

## Tests

- Unit tests sit next to the code. Integration tests are modules of one binary, `tests/it/main.rs`: a new area adds
  a module there, not a file in `tests/`.
- Fixtures under `tests/fixtures/` are byte-exact exports (CRLF, trailing spaces, other encodings);
  `.gitattributes` and `.editorconfig` leave their bytes alone.
- **Only synthetic data is committed:** `sqlca-gen` output or Oracle exports of synthetic data. A real export is run
  locally and never committed; what it teaches becomes a generator option and a synthetic fixture.
- Large generated exports (GB scale) go to `gen-out/`, which is git-ignored.
- Prove a fix by reverting it and watching a test fail.
