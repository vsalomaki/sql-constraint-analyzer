# sql-constraint-analyzer

`sqlca` checks Oracle SQL exports (table DDL, constraints and `INSERT` statements, as SQL Developer writes them) for
rows that violate the declared constraints, foreign keys above all, and can write fixed copies.

> **Status:** early development. `sqlca check`, `sqlca inputs` and `sqlca fix` (`--out DIR` or `--in-place`, with
> `--dry-run`) work; more fix policies are next.

## What it will do

Oracle comes first; other SQL dialects are meant to plug in the same way later (`--dialect`).

- Read DDL and data files of any size (GBs), streaming, in the order given.
- Build the schema from `CREATE TABLE`, `ALTER TABLE … ADD CONSTRAINT`, `CREATE UNIQUE INDEX`.
- Report every row that breaks a foreign key, a primary key or unique key (duplicates), or a NOT NULL column, with
  `file:line:col`, following Oracle's rules (`''` is NULL, an FK with a NULL column is satisfied, identifier case
  folding, constraint states such as `DISABLE` and `DEFERRABLE`).
- Optionally check the load order too: a child row inserted before its parent.
- Fix: write corrected files to another folder or in place, with every removed row kept in a rejects file and a
  report of each change.

```sh
sqlca check 'export/*.sql'                       # report violations
sqlca check 'app/**/*.sql' --reference 'ref/**/*.sql'  # parents from another schema, not checked
sqlca check --load-order --files-from order.txt  # also check the load order given
sqlca check 'export/*.sql' --format json > findings.jsonl  # JSON Lines; progress stays on stderr
sqlca fix 'export/*.sql' --out fixed/            # write fixed copies; removed rows in fixed/sqlca-report/rejects.sql
sqlca fix 'export/*.sql' --in-place --dry-run    # print what an in-place fix would change; write nothing
sqlca fix 'export/*.sql' --in-place              # rewrite the inputs, keeping <file>.orig and ./sqlca-fix-<time>/
sqlca inputs --script load.sql                   # show what would be read, and in what order
```

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

The repository holds only synthetic data: exports written by `sqlca-gen`, and exports of synthetic data made with
Oracle's own tools. Real exports are never committed.

## License

[Apache-2.0](LICENSE)
