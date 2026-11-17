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

/// P4 Task 1: `init` and `doctor` exist with their flags, and their help renders.
#[test]
fn init_doctor_help_render() {
    for (command, about, flags) in [
        (
            "init",
            "Set this project up for hostler (preview; --write applies)",
            &["--write", "--json"][..],
        ),
        (
            "doctor",
            "Check hostler and this project; --fix applies the safe fixes",
            &["--fix", "--json"],
        ),
    ] {
        let output = Command::cargo_bin("hostler")
            .unwrap()
            .args([command, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{command}: {output:?}");
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.starts_with(about), "{command}: {help}");
        for flag in flags {
            assert!(help.contains(flag), "{command} lacks {flag}: {help}");
        }
    }
    let output = Command::cargo_bin("hostler")
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8(output.stdout).unwrap();
    for command in ["init", "doctor"] {
        assert!(
            help.lines()
                .any(|line| line.trim_start().starts_with(command)),
            "the command list lacks {command}: {help}"
        );
    }
}

/// Removal test (P4-L6: MCP is out of P4): `hostler mcp` is not a command and the root help
/// lists none.
#[test]
fn mcp_command_is_removed() {
    let output = Command::cargo_bin("hostler")
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(
        !help
            .lines()
            .any(|line| line.trim_start().starts_with("mcp")),
        "{help}"
    );
    Command::cargo_bin("hostler")
        .unwrap()
        .args(["mcp", "--help"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unrecognized subcommand 'mcp'"));
}
