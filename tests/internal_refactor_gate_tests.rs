#![cfg(test)]

mod common;

#[test]
fn internal_refactor_declarations_bind_exact_blobs_and_change_scope() {
    let root = common::repo_root();
    let temporary = common::unique_temp_dir("internal-refactor-gate");
    std::fs::create_dir_all(&temporary).expect("create fixture directory");
    let output = std::process::Command::new("pwsh")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
        .arg(root.join(".github/test-fixtures/test-internal-refactor.ps1"))
        .arg("-RepositoryRoot")
        .arg(&root)
        .current_dir(&temporary)
        .output()
        .expect("run PowerShell gate fixtures");
    std::fs::remove_dir_all(&temporary).expect("remove fixture directory");
    assert!(
        output.status.success(),
        "internal-refactor policy failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
