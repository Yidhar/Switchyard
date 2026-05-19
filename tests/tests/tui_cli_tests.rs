use std::process::Command;

#[test]
fn tui_help_describes_resume_entrypoints() {
    let output = Command::new(env!("CARGO_BIN_EXE_switchyard-tui"))
        .arg("--help")
        .output()
        .expect("failed to run switchyard-tui --help");

    assert!(
        output.status.success(),
        "--help should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Interactive TUI for Switchyard"));
    assert!(stdout.contains("--provider"));
    assert!(stdout.contains("--session"));
    assert!(stdout.contains("--resume-latest"));
    assert!(stdout.contains("--cwd"));
}
