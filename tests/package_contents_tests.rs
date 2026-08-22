use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;

use common::repo_root;

const REPOSITORY_ONLY_TEST_MODULES: [&str; 8] = [
    "src/server/app_admission_tests.rs",
    "src/server/game_data_tests.rs",
    "src/server/message_coordinator_tests.rs",
    "src/server/message_router_tests.rs",
    "src/server/ready_state_tests.rs",
    "src/server/room_service_tests.rs",
    "src/server/session_policy_tests.rs",
    "src/server/signaling_tests.rs",
];

fn command_output(program: &str, args: &[&str]) -> Output {
    let output = Command::new(program)
        .args(args)
        // CI deliberately enables colored Cargo output. Keep nested command
        // diagnostics deterministic so warning classification compares text,
        // not terminal presentation escape sequences.
        .env("CARGO_TERM_COLOR", "never")
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

fn strict_cargo_output(args: &[&str]) -> Output {
    let output = Command::new("cargo")
        .args(args)
        .env("CARGO_TERM_COLOR", "never")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("RUSTFLAGS", "-D warnings")
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|error| panic!("failed to run strict cargo {args:?}: {error}"));

    assert!(
        output.status.success(),
        "strict cargo {args:?} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    output
}

fn compile_package_build_script(output_dir: &Path) -> PathBuf {
    let mut executable = output_dir.join("package-build-script-test");
    if cfg!(windows) {
        executable.set_extension("exe");
    }
    let output = Command::new("rustc")
        .arg(repo_root().join("build.rs"))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("rustc must execute while compiling the package build-script regression");
    assert!(
        output.status.success(),
        "build.rs regression fixture must compile:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    executable
}

fn normalize_package_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn is_cargo_lock_progress(line: &str) -> bool {
    matches!(
        line.trim_start(),
        "Blocking waiting for file lock on package cache"
            | "Blocking waiting for file lock on build directory"
    )
}

fn is_cargo_build_progress(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("Verifying signal-fish-server ")
        || trimmed.starts_with("Compiling ")
        || trimmed.starts_with("Checking ")
        || trimmed.starts_with("Finished `dev` profile ")
}

fn is_expected_package_diagnostic(line: &str) -> bool {
    let trimmed = line.trim_start();
    is_cargo_lock_progress(line)
        || is_cargo_build_progress(line)
        || line.starts_with("warning: ignoring test `")
        || line.starts_with("warning: ignoring benchmark `")
        || trimmed.starts_with("Packaging signal-fish-server ")
        || trimmed.starts_with("Updating crates.io index")
        || trimmed.starts_with("Packaged ")
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
        // Cargo renders native separators on Windows; compare the logical
        // archive inventory in Git's repository-relative path form.
        .map(normalize_package_path)
        .collect::<BTreeSet<_>>();

    let mut expected = command_stdout("git", &["ls-files", "src"])
        .lines()
        .filter(|path| !REPOSITORY_ONLY_TEST_MODULES.contains(path))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    expected.extend(
        [
            ".cargo_vcs_info.json",
            "build.rs",
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

    let list_diagnostics =
        String::from_utf8(package.stderr).expect("cargo package --list diagnostics must be UTF-8");
    let unexpected_list_diagnostics = list_diagnostics
        .lines()
        .filter(|line| !is_cargo_lock_progress(line))
        .collect::<Vec<_>>();
    assert!(
        unexpected_list_diagnostics.is_empty(),
        "cargo package --list emitted unexpected diagnostics: {unexpected_list_diagnostics:#?}"
    );
    assert_published_readme_links_resolve(&actual);

    let package_test_target = repo_root().join("target").join("package-contents-tests");
    let package_test_target = package_test_target
        .to_str()
        .expect("package test target path must be UTF-8");
    let packaged = strict_cargo_output(&[
        "package",
        "--locked",
        "--allow-dirty",
        "--all-features",
        "--target-dir",
        package_test_target,
    ]);
    let warnings = String::from_utf8(packaged.stderr).expect("cargo warnings must be UTF-8");
    let unexpected_warnings = warnings
        .lines()
        .filter(|line| !is_expected_package_diagnostic(line))
        .collect::<Vec<_>>();
    assert!(
        unexpected_warnings.is_empty(),
        "cargo package emitted unexpected warnings: {unexpected_warnings:#?}"
    );
    assert!(
        warnings.contains("is not included in the published package"),
        "Cargo must explain that repository-only test/benchmark targets are intentionally omitted"
    );

    let packaged_manifest = repo_root()
        .join("target")
        .join("package-contents-tests")
        .join("package")
        .join(format!(
            "{}-{}",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION")
        ))
        .join("Cargo.toml");
    let packaged_manifest = packaged_manifest
        .to_str()
        .expect("generated package manifest path must be UTF-8");
    strict_cargo_output(&[
        "test",
        "--locked",
        "--manifest-path",
        packaged_manifest,
        "--lib",
        "--all-features",
        "--no-run",
        "--target-dir",
        package_test_target,
    ]);
}

#[test]
fn package_paths_are_normalized_across_platforms() {
    assert_eq!(
        normalize_package_path("src/auth/error.rs"),
        "src/auth/error.rs"
    );
    assert_eq!(
        normalize_package_path("src\\auth\\error.rs"),
        "src/auth/error.rs"
    );
    assert_eq!(normalize_package_path("README.md"), "README.md");
}

#[test]
fn package_build_script_distinguishes_checkout_archive_and_partial_sources() {
    struct Case {
        name: &'static str,
        present_modules: usize,
        package_markers: &'static [&'static str],
        cargo_chef_skeleton: bool,
        succeeds: bool,
        enables_repository_tests: bool,
    }

    let cases = [
        Case {
            name: "complete-checkout",
            present_modules: REPOSITORY_ONLY_TEST_MODULES.len(),
            package_markers: &[],
            cargo_chef_skeleton: false,
            succeeds: true,
            enables_repository_tests: true,
        },
        Case {
            name: "generated-package",
            present_modules: 0,
            package_markers: &["Cargo.toml.orig", ".cargo_vcs_info.json"],
            cargo_chef_skeleton: false,
            succeeds: true,
            enables_repository_tests: false,
        },
        Case {
            name: "empty-checkout",
            present_modules: 0,
            package_markers: &[],
            cargo_chef_skeleton: false,
            succeeds: false,
            enables_repository_tests: false,
        },
        Case {
            name: "partial-generated-package",
            present_modules: 1,
            package_markers: &["Cargo.toml.orig", ".cargo_vcs_info.json"],
            cargo_chef_skeleton: false,
            succeeds: false,
            enables_repository_tests: false,
        },
        Case {
            name: "cargo-chef-skeleton",
            present_modules: 0,
            package_markers: &[],
            cargo_chef_skeleton: true,
            succeeds: true,
            enables_repository_tests: false,
        },
        Case {
            name: "cargo-toml-orig-only",
            present_modules: 0,
            package_markers: &["Cargo.toml.orig"],
            cargo_chef_skeleton: false,
            succeeds: false,
            enables_repository_tests: false,
        },
        Case {
            name: "cargo-vcs-info-only",
            present_modules: 0,
            package_markers: &[".cargo_vcs_info.json"],
            cargo_chef_skeleton: false,
            succeeds: false,
            enables_repository_tests: false,
        },
    ];

    let fixture = tempfile::tempdir().expect("build-script fixture directory must be created");
    let executable = compile_package_build_script(fixture.path());

    for case in cases {
        let manifest_dir = fixture.path().join(case.name);
        std::fs::create_dir_all(&manifest_dir).expect("case manifest directory must be created");
        for path in REPOSITORY_ONLY_TEST_MODULES
            .iter()
            .take(case.present_modules)
        {
            let path = manifest_dir.join(path);
            std::fs::create_dir_all(path.parent().expect("test module must have a parent"))
                .expect("test module parent must be created");
            std::fs::write(path, []).expect("test module fixture must be written");
        }
        for &marker in case.package_markers {
            std::fs::write(manifest_dir.join(marker), [])
                .expect("package marker fixture must be written");
        }

        let mut command = Command::new(&executable);
        command.env("CARGO_MANIFEST_DIR", &manifest_dir);
        if case.cargo_chef_skeleton {
            command.env("SIGNAL_FISH_CARGO_CHEF_SKELETON", "1");
            command.env("CARGO_PKG_VERSION", "0.0.1");
        } else {
            command.env_remove("SIGNAL_FISH_CARGO_CHEF_SKELETON");
            command.env("CARGO_PKG_VERSION", env!("CARGO_PKG_VERSION"));
        }
        let output = command.output().expect("build-script fixture must execute");
        assert_eq!(
            output.status.success(),
            case.succeeds,
            "build-script case `{}` emitted stderr:\n{}",
            case.name,
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("build-script output must be UTF-8");
        let mut rerun_paths = stdout
            .lines()
            .filter_map(|line| line.strip_prefix("cargo::rerun-if-changed="))
            .collect::<Vec<_>>();
        rerun_paths.sort_unstable();
        let mut expected_rerun_paths = vec!["build.rs"];
        expected_rerun_paths.extend(
            REPOSITORY_ONLY_TEST_MODULES
                .iter()
                .take(case.present_modules)
                .copied(),
        );
        expected_rerun_paths.extend(case.package_markers.iter().copied());
        expected_rerun_paths.sort_unstable();
        assert_eq!(
            rerun_paths, expected_rerun_paths,
            "build-script case `{}` must watch exactly its existing inputs:\n{stdout}",
            case.name
        );
        let rerun_env = stdout
            .lines()
            .filter_map(|line| line.strip_prefix("cargo::rerun-if-env-changed="))
            .collect::<Vec<_>>();
        assert_eq!(
            rerun_env,
            ["SIGNAL_FISH_CARGO_CHEF_SKELETON"],
            "build-script case `{}` must watch exactly the Cargo Chef classifier:\n{stdout}",
            case.name
        );
        assert_eq!(
            stdout.contains("cargo::rustc-cfg=signal_fish_repository_tests"),
            case.enables_repository_tests,
            "build-script case `{}` emitted unexpected cfg output:\n{stdout}",
            case.name
        );
    }
}

#[test]
fn cargo_package_diagnostics_classify_cache_contention_without_hiding_warnings() {
    let cases = [
        (
            "    Blocking waiting for file lock on package cache",
            true,
            "ordinary indented cache contention",
        ),
        (
            "Blocking waiting for file lock on package cache",
            true,
            "ordinary unindented cache contention",
        ),
        (
            "warning: ignoring test `integration_tests` as `tests/integration_tests.rs` is not included in the published package",
            true,
            "expected omitted test target",
        ),
        (
            "warning: ignoring benchmark `relay_allocations` as `benches/relay_allocations.rs` is not included in the published package",
            true,
            "expected omitted benchmark target",
        ),
        (
            "warning: package `signal-fish-server` has no documentation",
            false,
            "unrelated Cargo warning",
        ),
        (
            "Blocking waiting for file lock on build directory",
            true,
            "ordinary build contention",
        ),
        (
            "Blocking waiting for file lock on package cache forever",
            false,
            "cache-lock near miss",
        ),
        (
            "   Verifying signal-fish-server v0.7.0 (/tmp/package)",
            true,
            "package verification progress",
        ),
        (
            "   Compiling signal-fish-server v0.7.0 (/tmp/package)",
            true,
            "package compilation progress",
        ),
        (
            "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.00s",
            true,
            "package verification completion",
        ),
    ];

    for (line, expected, description) in cases {
        assert_eq!(
            is_expected_package_diagnostic(line),
            expected,
            "diagnostic case `{description}`: {line:?}"
        );
    }
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
