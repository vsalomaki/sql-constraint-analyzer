# sql-constraint-analyzer

`sqlca` checks SQL exports (table DDL, constraints and `INSERT` statements) for rows that violate the declared
constraints, foreign keys above all, and writes fixed copies.

> **Status:** early development. `sqlca check`, `sqlca inputs` and `sqlca fix` (`--out DIR` or `--in-place`, with
> `--dry-run`, `--on-duplicate keep-last`, `--on-fk null`, `--fix-assumed` and `--annotate`) work.

## What it does

- Reads DDL and data files of any size (GBs), streaming, in the order given.
- Builds the schema from `CREATE TABLE`, `ALTER TABLE … ADD CONSTRAINT` and `CREATE UNIQUE INDEX`.
- Reports every row that breaks a foreign key, a primary or unique key (duplicates), or a NOT NULL column, with
  `file:line`, by the database's own rules: which values count as NULL, how identifiers fold, how partly NULL keys
  compare, and constraint states such as disabled or deferred.
- Optionally checks the load order too: a child row inserted before its parent.
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
sqlca check 'export/*.sql' --no-commit-check     # don't require a COMMIT after each data file's rows
sqlca check 'export/*.sql' --format json > findings.jsonl  # JSON Lines; progress stays on stderr
sqlca fix 'export/*.sql' --out fixed/            # write fixed copies; removed rows in fixed/sqlca-report/rejects.sql
sqlca fix 'export/*.sql' --in-place --dry-run    # print what an in-place fix would change; write nothing
sqlca fix 'export/*.sql' --in-place              # rewrite the inputs, keeping <file>.orig and ./sqlca-fix-<time>/
sqlca fix 'export/*.sql' --out fixed/ --on-duplicate keep-last --annotate  # keep each key's last row; comment removals
sqlca fix 'export/*.sql' --out fixed/ --on-fk null  # keep rows with a missing parent, their foreign key set to NULL
sqlca inputs --script load.sql                   # show what would be read, and in what order
```

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
