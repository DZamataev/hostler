use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn version_prints_crate_version() {
    Command::cargo_bin("hostler")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::eq(format!(
            "hostler {}\n",
            env!("CARGO_PKG_VERSION")
        )));
}
