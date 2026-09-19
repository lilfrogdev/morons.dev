use std::process::Command;

#[test]
fn public_flags_reach_the_cli_instead_of_the_clipboard_helper() {
    let help = Command::new(env!("CARGO_BIN_EXE_morons"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("--debug-status")
    );
    for args in [["--debug", "extra"], ["--debug-status", "extra"]] {
        let invalid = Command::new(env!("CARGO_BIN_EXE_morons"))
            .args(args)
            .output()
            .unwrap();
        assert!(!invalid.status.success());
        assert!(
            String::from_utf8(invalid.stderr)
                .unwrap()
                .contains("Usage:")
        );
    }
}
