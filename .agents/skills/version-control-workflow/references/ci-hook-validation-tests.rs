// tests/ci_config_tests.rs

#[test]
fn test_git_hooks_are_executable() {
    let githooks_dir = repo_root().join(".githooks");
    if !githooks_dir.exists() {
        return;
    }

    for entry in std::fs::read_dir(&githooks_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() && path.extension().is_none() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).unwrap().permissions().mode();
                assert!(
                    mode & 0o111 != 0,
                    "{} is not executable.\nFix:\n  chmod +x {}\n  git update-index --chmod=+x {}",
                    path.display(),
                    path.display(),
                    path.display()
                );
            }
        }
    }
}

#[test]
fn test_hook_installation_script_exists() {
    let script = repo_root().join("scripts/enable-hooks.sh");
    assert!(
        script.exists(),
        "scripts/enable-hooks.sh is required for hook installation."
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&script).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "scripts/enable-hooks.sh must be executable.");
    }
}
