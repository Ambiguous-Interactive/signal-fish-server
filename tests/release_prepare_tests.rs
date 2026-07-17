#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const RELEASE_DATE: &str = "2026-07-17";

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new(version: &str) -> Self {
        let temp = tempfile::tempdir().expect("create release fixture");
        let root = temp.path().to_path_buf();
        for directory in [".llm", "clients/native", "docs", "fuzz", "scripts"] {
            fs::create_dir_all(root.join(directory)).expect("create fixture directory");
        }

        write(
            &root.join("Cargo.toml"),
            &format!(
                "[package]\nname = \"signal-fish-server\"\nversion = \"{version}\"\n\n\
                 [dependencies]\nexample = {{ version = \"9.9.9\" }}\n"
            ),
        );
        for lock in ["Cargo.lock", "clients/native/Cargo.lock"] {
            write(
                &root.join(lock),
                &format!(
                    "version = 4\n\n[[package]]\nname = \"example\"\nversion = \"9.9.9\"\n\n\
                     [[package]]\nname = \"signal-fish-server\"\nversion = \"{version}\"\n"
                ),
            );
        }
        write(
            &root.join("clients/native/Cargo.toml"),
            "[package]\nname = \"native\"\nversion = \"0.1.0\"\n",
        );
        write(
            &root.join("docs/library-usage.md"),
            &format!(
                "```toml\nsignal-fish-server = \"{version}\"\n\
                 signal-fish-server = {{ version = \"{version}\", features = [\"tls\"] }}\n```\n"
            ),
        );
        write(
            &root.join(".llm/context.md"),
            &format!("# Context\n\n- **Version:** {version}\n"),
        );
        write(
            &root.join("CHANGELOG.md"),
            "# Changelog\n\n\
             The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).\n\n\
             ## [Unreleased]\n\n\
             ### Added\n\n\
             - Ship the release preparation workflow.\n\n\
             ### Fixed\n\n\
             - Preserve categorized notes.\n\n\
             ## [1.1.0] - 2026-07-01\n\n\
             ### Added\n\n\
             - Previous release.\n\n\
             [Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v1.1.0...HEAD\n\
             [1.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v1.1.0\n",
        );
        write(
            &root.join("scripts/check-doc-consistency.sh"),
            "#!/usr/bin/env bash\nexit 0\n",
        );

        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()
            .expect("initialize fixture worktree");
        assert!(status.success(), "git init failed for release fixture");

        Self { _temp: temp, root }
    }

    fn run(&self, arguments: &[&str]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/prepare-release.sh");
        Command::new("bash")
            .arg(script)
            .args(arguments)
            .current_dir(&self.root)
            .env("PREPARE_RELEASE_CARGO_BIN", "true")
            .env("PREPARE_RELEASE_DOC_CHECK", "true")
            .output()
            .expect("run prepare-release.sh")
    }
}

fn write(path: &Path, content: &str) {
    fs::write(path, content).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn read(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn prepare_release_applies_every_semver_bump_and_synchronizes_release_files() {
    for (bump, expected, release_date) in [
        ("patch", "1.2.4", RELEASE_DATE),
        ("minor", "1.3.0", RELEASE_DATE),
        ("major", "2.0.0", "2028-02-29"),
    ] {
        let fixture = Fixture::new("1.2.3");
        let output = fixture.run(&["--bump", bump, "--date", release_date]);
        assert!(
            output.status.success(),
            "{bump} preparation failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let cargo_toml = read(fixture.root.join("Cargo.toml"));
        assert!(cargo_toml.contains(&format!("version = \"{expected}\"")));
        assert!(cargo_toml.contains("example = { version = \"9.9.9\" }"));

        for lock in ["Cargo.lock", "clients/native/Cargo.lock"] {
            let contents = read(fixture.root.join(lock));
            assert!(
                contents.contains(&format!(
                    "name = \"signal-fish-server\"\nversion = \"{expected}\""
                )),
                "{lock} did not receive {expected}:\n{contents}"
            );
            assert!(contents.contains("name = \"example\"\nversion = \"9.9.9\""));
        }

        let docs = read(fixture.root.join("docs/library-usage.md"));
        assert_eq!(docs.matches(expected).count(), 2);
        assert!(!docs.contains("1.2.3"));
        assert_eq!(
            read(fixture.root.join(".llm/context.md")),
            format!("# Context\n\n- **Version:** {expected}\n")
        );

        let changelog = read(fixture.root.join("CHANGELOG.md"));
        assert!(changelog.contains(&format!(
            "## [Unreleased]\n\n## [{expected}] - {release_date}\n\n### Added"
        )));
        assert!(changelog.contains("- Preserve categorized notes."));
        assert!(changelog.contains(&format!(
            "[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v{expected}...HEAD"
        )));
        assert!(changelog.contains(&format!(
            "[{expected}]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v1.2.3...v{expected}"
        )));
    }
}

#[test]
fn prepare_release_rejects_invalid_inputs_before_mutating_files() {
    for arguments in [
        vec!["--bump", "prerelease"],
        vec!["--bump", "patch", "--date", "2026-02-30"],
        vec!["--bump", "patch", "--date", "2025-02-29"],
        vec!["--bump", "patch", "--date", "0000-01-01"],
        vec!["--bump", "patch", "--date", "2026-13-01"],
        vec!["--bump", "patch", "--date", "2026-01-00"],
        vec!["--date", RELEASE_DATE],
    ] {
        let fixture = Fixture::new("1.2.3");
        let before = read(fixture.root.join("Cargo.toml"));
        let output = fixture.run(&arguments);
        assert!(
            !output.status.success(),
            "invalid arguments unexpectedly passed"
        );
        assert_eq!(read(fixture.root.join("Cargo.toml")), before);
    }
}

#[test]
fn prepare_release_fails_closed_on_empty_notes_existing_version_or_lock_drift() {
    let empty = Fixture::new("1.2.3");
    write(
        &empty.root.join("CHANGELOG.md"),
        "# Changelog\n\nKeep a Changelog\n\n## [Unreleased]\n\n\
         ## [1.2.3] - 2026-07-01\n\n### Added\n\n- Previous.\n\n\
         [Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v1.2.3...HEAD\n\
         [1.2.3]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v1.2.3\n",
    );
    let output = empty.run(&["--bump", "patch", "--date", RELEASE_DATE]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("categorized release notes"));

    let duplicate = Fixture::new("1.2.3");
    let changelog = read(duplicate.root.join("CHANGELOG.md")).replace(
        "## [1.1.0] - 2026-07-01",
        "## [1.2.4] - 2026-07-10\n\n### Fixed\n\n- Already cut.\n\n## [1.1.0] - 2026-07-01",
    );
    write(&duplicate.root.join("CHANGELOG.md"), &changelog);
    let output = duplicate.run(&["--bump", "patch", "--date", RELEASE_DATE]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("already contains"));

    let lock_drift = Fixture::new("1.2.3");
    write(
        &lock_drift.root.join("clients/native/Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"different-package\"\nversion = \"1.2.3\"\n",
    );
    let output = lock_drift.run(&["--bump", "patch", "--date", RELEASE_DATE]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("exactly one signal-fish-server"));
}

#[test]
fn prepare_release_rejects_semver_arithmetic_overflow() {
    let fixture = Fixture::new("9223372036854775807.0.0");
    let output = fixture.run(&["--bump", "major", "--date", RELEASE_DATE]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Cannot safely increment"));
}
