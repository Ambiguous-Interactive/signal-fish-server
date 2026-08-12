use std::collections::BTreeSet;
use std::process::{Command, Output};

mod common;

use common::repo_root;

fn command_output(program: &str, args: &[&str]) -> Output {
    let output = Command::new(program)
        .args(args)
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|error| panic!("failed to run {program} {args:?}: {error}"));

    assert!(
        output.status.success(),
        "{program} {args:?} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    output
}

fn command_stdout(program: &str, args: &[&str]) -> String {
    String::from_utf8(command_output(program, args).stdout).expect("command output must be UTF-8")
}

fn assert_published_readme_links_resolve(package: &BTreeSet<String>) {
    let readme =
        std::fs::read_to_string(repo_root().join("README.md")).expect("README.md must be readable");

    let mut targets = Vec::new();
    let mut remainder = readme.as_str();
    while let Some((_, after_marker)) = remainder.split_once("](") {
        let Some((target, after_target)) = after_marker.split_once(')') else {
            break;
        };
        remainder = after_target;
        targets.push(target);
    }

    for attribute in ["href=\"", "src=\""] {
        let mut remainder = readme.as_str();
        while let Some((_, after_attribute)) = remainder.split_once(attribute) {
            let Some((target, after_target)) = after_attribute.split_once('"') else {
                break;
            };
            remainder = after_target;
            targets.push(target);
        }
    }

    for line in readme.lines() {
        if let Some((_, target)) = line
            .strip_prefix('[')
            .and_then(|line| line.split_once("]: "))
        {
            targets.push(
                target
                    .split_ascii_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim_matches(['<', '>']),
            );
        }
    }

    let unresolved = targets
        .into_iter()
        .filter(|target| {
            let path = target.split('#').next().unwrap_or_default();
            !path.is_empty()
                && !path.starts_with("https://")
                && !path.starts_with("http://")
                && !path.starts_with("mailto:")
                && !package.contains(path)
        })
        .collect::<Vec<_>>();

    assert!(
        unresolved.is_empty(),
        "published README has package-relative links to omitted files: {unresolved:#?}"
    );
}

#[test]
fn published_crate_contains_only_runtime_sources_and_metadata() {
    let package = command_output("cargo", &["package", "--locked", "--allow-dirty", "--list"]);
    let actual = String::from_utf8(package.stdout)
        .expect("cargo package output must be UTF-8")
        .lines()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();

    let mut expected = command_stdout("git", &["ls-files", "src"])
        .lines()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    expected.extend(
        [
            ".cargo_vcs_info.json",
            "Cargo.lock",
            "Cargo.toml",
            "Cargo.toml.orig",
            "LICENSE",
            "README.md",
            "config.example.json",
        ]
        .map(str::to_owned),
    );

    let unexpected = actual.difference(&expected).cloned().collect::<Vec<_>>();
    let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();

    assert!(
        unexpected.is_empty() && missing.is_empty(),
        "published crate contents drifted from the minimal allowlist\n\
         unexpected files: {unexpected:#?}\n\
        missing files: {missing:#?}"
    );

    assert!(
        package.stderr.is_empty(),
        "cargo package --list should not emit diagnostics: {}",
        String::from_utf8_lossy(&package.stderr)
    );
    assert_published_readme_links_resolve(&actual);

    let packaged = command_output(
        "cargo",
        &[
            "package",
            "--locked",
            "--allow-dirty",
            "--all-features",
            "--no-verify",
        ],
    );
    let warnings = String::from_utf8(packaged.stderr).expect("cargo warnings must be UTF-8");
    let unexpected_warnings = warnings
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !(line.starts_with("warning: ignoring test `")
                || line.starts_with("warning: ignoring benchmark `")
                || trimmed.starts_with("Packaging signal-fish-server ")
                || trimmed.starts_with("Updating crates.io index")
                || trimmed.starts_with("Packaged "))
        })
        .collect::<Vec<_>>();
    assert!(
        unexpected_warnings.is_empty(),
        "cargo package emitted unexpected warnings: {unexpected_warnings:#?}"
    );
    assert!(
        warnings.contains("is not included in the published package"),
        "Cargo must explain that repository-only test/benchmark targets are intentionally omitted"
    );
}

#[test]
fn progress_notes_remain_local_only() {
    let tracked = command_stdout("git", &["ls-files", "progress"]);
    assert!(
        tracked.is_empty(),
        "progress notes must not be tracked; remove these paths from the index:\n{tracked}"
    );

    let ignored = command_stdout(
        "git",
        &[
            "check-ignore",
            "--no-index",
            "--verbose",
            "progress/session-local.md",
        ],
    );
    assert!(
        ignored.starts_with(".gitignore:"),
        "progress/session-local.md must be ignored by the repository .gitignore, got: {ignored}"
    );
}
