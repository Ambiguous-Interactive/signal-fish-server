//! Contract tests for the portable Agent Skills layout in `.llm/skills`.

mod common;

use std::process::Command;

use common::repo_root;

fn run_skill_script(script: &str, args: &[&str]) -> std::process::Output {
    Command::new("python3")
        .arg(repo_root().join(script))
        .args(args)
        .current_dir(repo_root())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap_or_else(|error| panic!("failed to run {script}: {error}"))
}

#[test]
fn portable_skill_library_is_valid() {
    let output = run_skill_script(".llm/skills/manage-skills/scripts/validate_skills.py", &[]);
    assert!(
        output.status.success(),
        "skill validation failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn generated_skill_catalog_is_fresh() {
    let output = run_skill_script(
        ".llm/skills/manage-skills/scripts/generate_skills_index.py",
        &["--check"],
    );
    assert!(
        output.status.success(),
        "skill catalog check failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
