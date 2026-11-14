#![cfg(test)]

mod common;

#[test]
fn internal_refactor_declarations_bind_exact_blobs_and_change_scope_when_pwsh_available() {
    let root = common::repo_root();
    let result = std::process::Command::new("pwsh")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
        .arg(root.join(".github/test-fixtures/test-internal-refactor.ps1"))
        .arg("-RepositoryRoot")
        .arg(&root)
        .current_dir(&root)
        .output();
    let output = match result {
        Ok(output) => output,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && std::env::var_os("CI").is_none() =>
        {
            eprintln!("skipping PowerShell gate fixtures: pwsh is unavailable locally");
            return;
        }
        Err(error) => panic!("run PowerShell gate fixtures: {error}"),
    };
    assert!(
        output.status.success(),
        "internal-refactor policy failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn rust_documentation_gate_checks_literals_and_both_git_modes_when_pwsh_available() {
    let root = common::repo_root();
    let result = std::process::Command::new("pwsh")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
        .arg(root.join(".github/test-fixtures/test-rust-documentation.ps1"))
        .arg("-RepositoryRoot")
        .arg(&root)
        .current_dir(&root)
        .output();
    let output = match result {
        Ok(output) => output,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && std::env::var_os("CI").is_none() =>
        {
            return
        }
        Err(error) => panic!("run Rust documentation gate fixtures: {error}"),
    };
    assert!(
        output.status.success(),
        "Rust documentation gate failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
