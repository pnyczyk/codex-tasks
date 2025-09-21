use assert_cmd::Command;

const BIN: &str = "codex-tasks";

#[test]
fn help_lists_supported_subcommands() {
    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.arg("--help");
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("start"))
        .stdout(predicates::str::contains("send"))
        .stdout(predicates::str::contains("status"))
        .stdout(predicates::str::contains("log"))
        .stdout(predicates::str::contains("stop"))
        .stdout(predicates::str::contains("ls"))
        .stdout(predicates::str::contains("archive"));
}

#[test]
fn subcommands_return_not_implemented_errors() {
    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.arg("start");
    cmd.assert()
        .failure()
        .stderr(predicates::str::contains("`start` is not implemented yet"));

    let mut cmd = Command::cargo_bin(BIN).expect("binary should build");
    cmd.args(["ls", "--state", "RUNNING"]);
    cmd.assert()
        .failure()
        .stderr(predicates::str::contains("`ls` is not implemented yet"));
}
