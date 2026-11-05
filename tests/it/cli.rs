use assert_cmd::Command;
use predicates::str::contains;

#[expect(
    clippy::unwrap_used,
    reason = "a missing binary is a broken build, not a test result"
)]
fn sqlca() -> Command {
    Command::cargo_bin("sqlca").unwrap()
}

#[test]
fn help_lists_the_subcommands() {
    sqlca()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("check"))
        .stdout(contains("fix"))
        .stdout(contains("inputs"));
}

#[test]
fn version_is_the_package_version() {
    sqlca()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn no_subcommand_is_a_usage_error() {
    sqlca().assert().code(2).stderr(contains("Usage"));
}
