# sql-constraint-analyzer

`sqlca` checks SQL exports (table DDL, constraints and `INSERT` statements) for rows that violate the declared
constraints, foreign keys above all, and writes fixed copies.

> **Status:** early development. `sqlca check`, `sqlca inputs` and `sqlca fix` (`--out DIR` or `--in-place`, with
> `--dry-run`, `--on-duplicate keep-last`, `--on-fk null`, `--fix-assumed`, `--annotate` and `--max-rounds`) work.

## What it does

- Reads DDL and data files of any size (GBs), streaming, in the order given.
- Builds the schema from `CREATE TABLE`, `ALTER TABLE … ADD CONSTRAINT` and `CREATE UNIQUE INDEX`.
- Reports every row that breaks a foreign key, a primary or unique key (duplicates), or a NOT NULL column, with
  `file:line`, by the database's own rules: which values count as NULL, how identifiers fold, how partly NULL keys
  compare, and constraint states such as disabled or deferred.
- Optionally checks the load order too: a child row inserted before its parent. `sqlca inputs --dependency-order`
  lists the data files parents-first, for `--files-from`, when the file order is the cause.
- Checks several exports at once: an unqualified table name means its folder's schema first (`a/x.sql` is in
  schema A), so two exports with the same table names stay apart. `--folder-schema top` takes the first folder
  instead (for `a/tables/`, `a/data/`), `off` turns it off, and `--default-schema` overrides it. A table created
  twice stops the run.
- Reports rows at the end of a data file with no COMMIT after them (or a statement that commits, such as DDL in
  Oracle). `--no-commit-check` turns this off.
- Fixes: writes corrected files to another folder or in place, with every removed or changed row kept in a rejects
  file and a report of each change, and adds a COMMIT where a data file's last rows have none. Where a constraint
  is checked only at commit (a deferred one), it adds none, since an early COMMIT can roll the load back, and leaves
  those findings for review.

Oracle is the supported dialect today. Others are meant to plug in the same way.

```sh
sqlca check 'export/*.sql'                       # report violations
sqlca check 'app/**/*.sql' --reference 'ref/**/*.sql'  # parents from another schema, not checked
sqlca check --load-order --files-from order.txt  # also check the load order given
sqlca check 'exports/**/*.sql' --folder-schema top  # exports/a/tables/…, exports/b/data/…: schemas A and B
sqlca check 'export/*.sql' --no-commit-check     # don't require a COMMIT after each data file's rows
sqlca check 'export/*.sql' --format json > findings.jsonl  # JSON Lines; progress stays on stderr
sqlca fix 'export/*.sql' --out fixed/            # write fixed copies; removed rows in fixed/sqlca-report/rejects.sql
sqlca fix 'export/*.sql' --in-place --dry-run    # print what an in-place fix would change; write nothing
sqlca fix 'export/*.sql' --in-place              # rewrite the inputs, keeping <file>.orig and ./sqlca-fix-<time>/
sqlca fix 'export/*.sql' --out fixed/ --on-duplicate keep-last --annotate  # keep each key's last row; comment removals
sqlca fix 'export/*.sql' --out fixed/ --on-fk null  # keep rows with a missing parent, their foreign key set to NULL
sqlca inputs --script load.sql                   # show what would be read, and in what order
sqlca inputs 'export/*.sql' --dependency-order --format paths > order.txt  # data files parents-first
```

## Giving the inputs

`check`, `fix` and `inputs` read their files in the order given. Order matters: a constraint applies from where it
is declared, and `--load-order` judges rows in that order. Name the main inputs in one of three ways:

| Form | Order | Example |
|---|---|---|
| Paths or glob patterns | Each pattern's matches version-sorted (`2_a.sql` before `10_a.sql`); patterns in the order given | `sqlca check 'export/tables/*.sql' 'export/data/**/*.sql'` |
| `--files-from LIST` | As listed, one literal path per line; `-` reads the list from stdin, `-0` takes NUL separators | `find export -name '*.sql' \| sort \| sqlca check --files-from -` |
| `--script FILE` | A SQL\*Plus master script's `@`, `@@` and `START` lines, followed recursively | `sqlca check --script export/load.sql` |

- Quote glob patterns: `sqlca` expands them itself, including `**`. A pattern that matches nothing, or a missing
  file, stops the run.
- In a `--files-from` list, entries are relative to the working directory, blank lines are skipped and CRLF line
  ends are fine. File names with spaces or newlines need `-0`: `find export -name '*.sql' -print0 | sqlca check
  --files-from - -0`.
- `--schema PATHS` adds DDL files read before everything else. `--reference PATHS` adds another schema's DDL and
  data, which supply parent keys and are not checked themselves. Both take every value that follows, so give them
  last or write `--schema=FILE`.
- `--encoding windows-1252` reads files that aren't UTF-8. `--runner sqlcl` splits statements as SQLcl and SQL
  Developer do, at any `;` outside strings and comments; the default follows SQL\*Plus.

`sqlca inputs` takes the same options and shows what a run would read: each file's position, role, size and why it
is there, as text or `--format json`.

### Loading parents first

When `check --load-order` reports children inserted before their parents because of the file order,
`--dependency-order` lists the data files so that each comes after the files holding the rows its foreign keys point
at. `--format paths` prints only those paths, ready for `--files-from`. Pass `--schema` and `--reference` again:

```sh
sqlca inputs 'export/**/*.sql' --dependency-order --format paths > order.txt
sqlca check --load-order --files-from order.txt

sqlca inputs 'export/**/*.sql' --dependency-order --print0 | sqlca check --load-order --files-from - -0
```

A file with table DDL in it keeps its place, and no file moves across it, since where a constraint is declared
decides which rows it judges: with SQL Developer's layout, the files in `data/` are reordered among themselves.
Files that don't depend on each other keep their order, and so do files inserting into the same table, since the
first row of a key is the one a duplicate leaves in place. A cycle is broken at the earliest file given in it. A key
whose parent rows still come later is named in a warning. Rows within one file are never reordered.

## Install

Download the archive for your platform from [Releases](https://github.com/vsalomaki/sql-constraint-analyzer/releases)
and put `sqlca` on your `PATH`.

| Platform | Target |
|---|---|
| Linux x86_64 / aarch64, glibc 2.35+ | `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` |
| Linux x86_64 / aarch64, static | `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` |
| macOS aarch64 | `aarch64-apple-darwin` |
| Windows x86_64 | `x86_64-pc-windows-msvc` |

On Linux, pick `gnu`: it is faster (`check` runs about a third slower on musl). Pick `musl` where `gnu` won't run,
such as Alpine or a distribution older than glibc 2.35; it is static and runs anywhere.

Check the download against `SHA256SUMS` and the build provenance GitHub signed:

```sh
sha256sum --check --ignore-missing SHA256SUMS       # macOS: shasum -a 256 -c --ignore-missing SHA256SUMS
gh attestation verify sqlca-<version>-<target>.tar.gz --repo vsalomaki/sql-constraint-analyzer
```

The macOS binary is not notarized: if Gatekeeper blocks it, `xattr -d com.apple.quarantine sqlca`.

## Building

Rust 1.98.1 (pinned in `rust-toolchain.toml`; rustup picks it up).

```sh
cargo build --release            # target/release/sqlca
cargo build --release -F gen     # also target/release/sqlca-gen, the test-data generator
```

Development needs [cargo-nextest](https://nexte.st) and [cargo-deny](https://github.com/EmbarkStudios/cargo-deny):

```sh
make lint    # rustfmt check, cargo deny, clippy with warnings as errors
make test    # cargo nextest
```

Once per clone, `make install-hooks` turns on a pre-push hook that scans outgoing commits for secrets with
[gitleaks](https://github.com/gitleaks/gitleaks#installation). `git push --no-verify` skips it (discouraged).

## Test data

The repository holds only synthetic data: exports written by `sqlca-gen`, and exports of synthetic data made with the
database's own tools. Real exports are never committed.

## License

[Apache-2.0](LICENSE)
