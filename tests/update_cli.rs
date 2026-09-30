//! Update refuses unowned installations without requiring relay credentials.

use std::process::Command;

fn update(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_buzzx"))
        .arg("update")
        .args(args)
        .current_dir(std::env::temp_dir())
        .env("BUZZX_CONFIG", "/nonexistent/buzzx-update-config")
        .env_remove("BUZZ_PRIVATE_KEY")
        .env_remove("BUZZ_AUTH_TAG")
        .output()
        .unwrap()
}

#[test]
fn absent_checkout_blocks_every_source_action_without_credentials() {
    for args in [&[][..], &["--check"][..], &["--plan"][..]] {
        let output = update(args);
        assert_eq!(output.status.code(), Some(1));
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["status"], "blocked");
        assert_eq!(report["restart_required"], false);
        assert!(report["target_commit"].is_null());
    }
}

#[test]
fn mutually_exclusive_actions_are_rejected_before_running_update() {
    let output = update(&["--check", "--plan"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}
