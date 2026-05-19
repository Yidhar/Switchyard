use std::process::Command;

#[test]
fn top_level_help_lists_tui_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_switchyard"))
        .arg("--help")
        .output()
        .expect("failed to run switchyard --help");

    assert!(
        output.status.success(),
        "--help should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("tui"),
        "top-level help should list the tui subcommand. stdout:\n{stdout}",
    );
}

#[test]
fn tui_help_describes_launch_command_and_overrides() {
    let output = Command::new(env!("CARGO_BIN_EXE_switchyard"))
        .args(["tui", "--help"])
        .output()
        .expect("failed to run switchyard tui --help");

    assert!(
        output.status.success(),
        "tui --help should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Launch the interactive TUI"));
    assert!(stdout.contains("--provider"));
    assert!(stdout.contains("--session"));
    assert!(stdout.contains("--resume-latest"));
    assert!(stdout.contains("--cwd"));
}
