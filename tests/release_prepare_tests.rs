#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
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
        for directory in [
            ".llm/code-samples/protocol",
            "clients/native",
            "clients/native/src",
            "docs/guides",
            "fuzz",
            "fuzz/src",
            "scripts",
            "src",
        ] {
            fs::create_dir_all(root.join(directory)).expect("create fixture directory");
        }

        write(
            &root.join("Cargo.toml"),
            &format!(
                "[package]\nname = \"signal-fish-server\"\nversion = \"{version}\"\n\
                 description = \"Fixture marker 9.9.9\"\n"
            ),
        );
        for lock in ["Cargo.lock", "clients/native/Cargo.lock", "fuzz/Cargo.lock"] {
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
            "[package]\nname = \"native\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nsignal-fish-server = { path = \"../..\" }\n",
        );
        write(
            &root.join("fuzz/Cargo.toml"),
            "[package]\nname = \"signal-fish-server-fuzz\"\nversion = \"0.0.0\"\n\n\
             [dependencies]\nsignal-fish-server = { path = \"..\" }\n",
        );
        for source in ["src/lib.rs", "clients/native/src/lib.rs", "fuzz/src/lib.rs"] {
            write(&root.join(source), "");
        }
        write(
            &root.join("docs/library-usage.md"),
            &format!(
                "```toml\nsignal-fish-server = \"{version}\"\n\
                 signal-fish-server = {{ version = \"{version}\", features = [\"tls\"] }}\n```\n"
            ),
        );
        write(
            &root.join(".llm/context.md"),
            &format!(
                "# Context\n\n- **Version:** {version}\n\n\
                 [v2 client sample](code-samples/protocol/v2-client-messages.jsonl)\n\
                 [v2 server sample](code-samples/protocol/v2-server-messages.jsonl)\n"
            ),
        );
        write(
            &root.join("CHANGELOG.md"),
            &format!("# Changelog\n\n\
             The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).\n\n\
             ## [Unreleased]\n\n\
             ### Added\n\n\
             - Ship the release preparation workflow.\n\n\
             ### Fixed\n\n\
             - Preserve categorized notes.\n\n\
             ## [{version}] - 2026-07-01\n\n\
             ### Added\n\n\
             - Previous release.\n\n\
             [Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v{version}...HEAD\n\
             [{version}]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v{version}\n"),
        );
        write(
            &root.join("README.md"),
            "# README\n\n[v2 client sample](.llm/code-samples/protocol/v2-client-messages.jsonl)\n[v2 server sample](.llm/code-samples/protocol/v2-server-messages.jsonl)\n",
        );
        write(
            &root.join(".llm/code-samples/protocol/v2-client-messages.jsonl"),
            "{\"type\":\"Authenticate\",\"data\":{}}\n",
        );
        write(
            &root.join(".llm/code-samples/protocol/v2-server-messages.jsonl"),
            "{\"type\":\"Authenticated\",\"data\":{\"app_name\":\"test\",\"rate_limits\":{}}}\n",
        );
        write(
            &root.join("docs/guides/rust-client.md"),
            "# Rust client\n\n```rust\npub enum GameDataEncoding {\n    Json,\n    MessagePack,\n}\n```\n",
        );
        let checker =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-doc-consistency.sh");
        let checker_destination = root.join("scripts/check-doc-consistency.sh");
        fs::copy(&checker, &checker_destination).expect("copy real document checker");
        let mut permissions = fs::metadata(&checker_destination)
            .expect("read checker metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&checker_destination, permissions)
            .expect("make fixture checker executable");

        for arguments in [
            vec!["init", "--quiet"],
            vec!["config", "user.name", "Release Fixture"],
            vec!["config", "user.email", "release-fixture@example.invalid"],
            vec!["add", "."],
            vec!["commit", "--quiet", "-m", "released baseline"],
        ] {
            let status = git_at(&root)
                .args(arguments)
                .status()
                .expect("initialize fixture release history");
            assert!(status.success(), "fixture Git setup failed");
        }
        let status = git_at(&root)
            .args(["tag", "-a"])
            .arg(format!("v{version}"))
            .args(["-m", "released baseline"])
            .status()
            .expect("create fixture release tag");
        assert!(status.success(), "fixture release tag setup failed");

        Self { _temp: temp, root }
    }

    fn run(&self, arguments: &[&str]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/prepare-release.sh");
        Command::new("bash")
            .arg(script)
            .args(arguments)
            .current_dir(&self.root)
            .env_remove("GIT_INDEX_FILE")
            .env("PREPARE_RELEASE_CARGO_BIN", "true")
            .env("PREPARE_RELEASE_DOC_CHECK", "true")
            .output()
            .expect("run prepare-release.sh")
    }

    fn run_with_real_doc_checker(&self, arguments: &[&str]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/prepare-release.sh");
        Command::new("bash")
            .arg(script)
            .args(arguments)
            .current_dir(&self.root)
            .env_remove("GIT_INDEX_FILE")
            .env("PREPARE_RELEASE_CARGO_BIN", "true")
            .output()
            .expect("run prepare-release.sh with real document checker")
    }

    fn run_with_cargo_bin(&self, arguments: &[&str], cargo_bin: &Path) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/prepare-release.sh");
        Command::new("bash")
            .arg(script)
            .args(arguments)
            .current_dir(&self.root)
            .env_remove("GIT_INDEX_FILE")
            .env("PREPARE_RELEASE_CARGO_BIN", cargo_bin)
            .env("PREPARE_RELEASE_DOC_CHECK", "true")
            .output()
            .expect("run prepare-release.sh with controlled Cargo")
    }

    fn run_with_actual_cargo(&self, arguments: &[&str]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/prepare-release.sh");
        Command::new("bash")
            .arg(script)
            .args(arguments)
            .current_dir(&self.root)
            .env_remove("GIT_INDEX_FILE")
            .env("PREPARE_RELEASE_DOC_CHECK", "true")
            .output()
            .expect("run prepare-release.sh with actual Cargo")
    }
}

fn git_at(root: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(root).env_remove("GIT_INDEX_FILE");
    command
}

fn write(path: &Path, content: &str) {
    fs::write(path, content).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn read(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

const RELEASE_FILES: [&str; 7] = [
    "Cargo.toml",
    "Cargo.lock",
    "clients/native/Cargo.lock",
    "fuzz/Cargo.lock",
    "CHANGELOG.md",
    "docs/library-usage.md",
    ".llm/context.md",
];

fn release_file_snapshot(fixture: &Fixture) -> Vec<(&'static str, String)> {
    RELEASE_FILES
        .iter()
        .map(|path| (*path, read(fixture.root.join(path))))
        .collect()
}

fn assert_release_files_unchanged(fixture: &Fixture, before: &[(&str, String)]) {
    for (path, expected) in before {
        assert_eq!(
            read(fixture.root.join(path)),
            *expected,
            "{path} changed despite a preflight failure"
        );
    }
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
        assert!(cargo_toml.contains("description = \"Fixture marker 9.9.9\""));

        for lock in ["Cargo.lock", "clients/native/Cargo.lock", "fuzz/Cargo.lock"] {
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
            format!(
                "# Context\n\n- **Version:** {expected}\n\n\
                 [v2 client sample](code-samples/protocol/v2-client-messages.jsonl)\n\
                 [v2 server sample](code-samples/protocol/v2-server-messages.jsonl)\n"
            )
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
fn prepare_release_supported_bumps_pass_real_cargo_resolution() {
    for (bump, expected) in [("patch", "1.2.4"), ("minor", "1.3.0"), ("major", "2.0.0")] {
        let fixture = Fixture::new("1.2.3");
        for manifest in ["Cargo.toml", "clients/native/Cargo.toml", "fuzz/Cargo.toml"] {
            let generated = Command::new("cargo")
                .args(["generate-lockfile", "--manifest-path", manifest])
                .current_dir(&fixture.root)
                .env_remove("GIT_INDEX_FILE")
                .output()
                .expect("generate realistic fixture lockfile");
            assert!(
                generated.status.success(),
                "failed to generate {manifest}:\n{}",
                String::from_utf8_lossy(&generated.stderr)
            );
        }
        let output = fixture.run_with_actual_cargo(&["--bump", bump, "--date", RELEASE_DATE]);
        assert!(
            output.status.success(),
            "{bump} preparation failed real Cargo resolution:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let check = Command::new("cargo")
            .args(["check", "--locked", "--manifest-path", "fuzz/Cargo.toml"])
            .current_dir(&fixture.root)
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("run real locked fuzz check");
        assert!(
            check.status.success(),
            "{bump} prepared fuzz graph rejected {expected}:\n{}",
            String::from_utf8_lossy(&check.stderr)
        );
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
        "## [1.2.3] - 2026-07-01",
        "## [1.2.4] - 2026-07-10\n\n### Fixed\n\n- Already cut.\n\n## [1.2.3] - 2026-07-01",
    );
    write(&duplicate.root.join("CHANGELOG.md"), &changelog);
    let output = duplicate.run(&["--bump", "patch", "--date", RELEASE_DATE]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("already contains"));

    for lockfile in ["clients/native/Cargo.lock", "fuzz/Cargo.lock"] {
        let lock_drift = Fixture::new("1.2.3");
        write(
            &lock_drift.root.join(lockfile),
            "version = 4\n\n[[package]]\nname = \"different-package\"\nversion = \"1.2.3\"\n",
        );
        let output = lock_drift.run(&["--bump", "patch", "--date", RELEASE_DATE]);
        assert!(
            !output.status.success(),
            "{lockfile} drift unexpectedly passed"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("exactly one signal-fish-server"),
            "unexpected {lockfile} diagnostic:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn prepare_release_discovers_future_tracked_standalone_lockfiles() {
    let fixture = Fixture::new("1.2.3");
    let package = fixture.root.join("tools/replay-client");
    fs::create_dir_all(&package).expect("create future standalone package");
    write(
        &package.join("Cargo.toml"),
        "[package]\nname = \"replay-client\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nsignal-fish-server = { path = \"../..\" }\n",
    );
    write(
        &package.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"signal-fish-server\"\nversion = \"1.2.3\"\n",
    );
    assert!(git_at(&fixture.root)
        .args(["add", "tools/replay-client"])
        .status()
        .expect("track future standalone package")
        .success());
    assert!(git_at(&fixture.root)
        .args(["commit", "--quiet", "-m", "add future standalone package"])
        .status()
        .expect("commit future standalone package")
        .success());

    let output = fixture.run(&["--bump", "patch", "--date", RELEASE_DATE]);

    assert!(
        output.status.success(),
        "future standalone lockfile was not prepared:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(read(package.join("Cargo.lock"))
        .contains("name = \"signal-fish-server\"\nversion = \"1.2.4\""));
}

#[test]
fn release_lockfile_discovery_and_rewrite_distinguish_path_registry_and_lookalikes() {
    let fixture = Fixture::new("1.2.3");
    for directory in ["registry", "mixed", "nested"] {
        fs::create_dir_all(fixture.root.join(directory)).expect("create lockfile case");
        write(
            &fixture.root.join(directory).join("Cargo.toml"),
            "[package]\nname = \"case\"\nversion = \"0.1.0\"\n",
        );
    }
    let registry_block = " [[package]] \n source=\"registry+https://github.com/rust-lang/crates.io-index\"\n name=\"signal-fish-server\"\n version=\"9.9.9\"\n checksum = \"abc\"\n";
    let path_block = "  [[package]] # local table\n  name = \"signal-fish-server\" # local package\n\tversion= \"1.2.3\" # local version\n";
    write(
        &fixture.root.join("registry/Cargo.lock"),
        &format!("version = 4\n\n{registry_block}"),
    );
    write(
        &fixture.root.join("mixed/Cargo.lock"),
        &format!("version = 4\n\n{registry_block}\n{path_block}"),
    );
    write(
        &fixture.root.join("nested/stale-Cargo.lock"),
        &format!("version = 4\n\n{path_block}"),
    );
    assert!(git_at(&fixture.root)
        .args(["add", "registry", "mixed", "nested"])
        .status()
        .expect("track lockfile cases")
        .success());
    assert!(git_at(&fixture.root)
        .args(["commit", "--quiet", "-m", "add lockfile identity cases"])
        .status()
        .expect("commit lockfile cases")
        .success());

    let list_script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/list-release-lockfiles.sh");
    let listed = Command::new("bash")
        .arg(list_script)
        .current_dir(&fixture.root)
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("list release lockfiles");
    assert!(listed.status.success());
    let listed: Vec<_> = listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect();
    assert!(listed.iter().any(|path| path == "mixed/Cargo.lock"));
    assert!(!listed.iter().any(|path| path == "registry/Cargo.lock"));
    assert!(!listed.iter().any(|path| path == "nested/stale-Cargo.lock"));

    let output = fixture.run(&["--bump", "patch", "--date", RELEASE_DATE]);
    assert!(
        output.status.success(),
        "mixed release preparation failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mixed = read(fixture.root.join("mixed/Cargo.lock"));
    assert!(
        mixed.contains(registry_block),
        "registry block changed:\n{mixed}"
    );
    assert!(mixed.contains(
        "  name = \"signal-fish-server\" # local package\n\tversion= \"1.2.4\" # local version"
    ));
    assert!(read(fixture.root.join("registry/Cargo.lock")).contains(registry_block));
}

#[test]
fn prepare_release_rolls_back_every_file_when_postflight_fails() {
    let fixture = Fixture::new("1.2.3");
    let before = release_file_snapshot(&fixture);
    let fake_cargo = fixture.root.join("fail-postflight-cargo.sh");
    let counter = fixture.root.join("cargo-call-count");
    write(&counter, "0\n");
    write(
        &fake_cargo,
        &format!(
            "#!/usr/bin/env bash\nset -euo pipefail\ncount=$(cat '{}')\ncount=$((count + 1))\nprintf '%s\\n' \"$count\" > '{}'\n[ \"$count\" -le 3 ]\n",
            counter.display(),
            counter.display()
        ),
    );
    let mut permissions = fs::metadata(&fake_cargo)
        .expect("fake Cargo metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_cargo, permissions).expect("make fake Cargo executable");

    let output =
        fixture.run_with_cargo_bin(&["--bump", "patch", "--date", RELEASE_DATE], &fake_cargo);

    assert!(
        !output.status.success(),
        "postflight failure unexpectedly passed"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("restored every release file"));
    assert_release_files_unchanged(&fixture, &before);
}

#[test]
fn prepare_release_rejects_semver_arithmetic_overflow() {
    let fixture = Fixture::new("9223372036854775807.0.0");
    let output = fixture.run(&["--bump", "major", "--date", RELEASE_DATE]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Cannot safely increment"));
}

#[test]
fn prepare_release_rejects_cargo_version_without_matching_latest_release_before_mutation() {
    let fixture = Fixture::new("1.2.3");
    let changelog = read(fixture.root.join("CHANGELOG.md"))
        .replace("[1.2.3]", "[1.1.0]")
        .replace("v1.2.3", "v1.1.0");
    write(&fixture.root.join("CHANGELOG.md"), &changelog);
    let before = release_file_snapshot(&fixture);

    let output = fixture.run(&["--bump", "patch", "--date", RELEASE_DATE]);

    assert!(!output.status.success(), "invalid release baseline passed");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("latest dated CHANGELOG.md release"),
        "unexpected diagnostic:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_release_files_unchanged(&fixture, &before);
}

#[test]
fn prepare_release_rejects_invalid_tag_baselines_before_mutation() {
    for tag_kind in ["missing", "lightweight", "non-ancestor", "target-exists"] {
        let fixture = Fixture::new("1.2.3");
        if tag_kind != "target-exists" {
            let status = git_at(&fixture.root)
                .args(["tag", "--delete", "v1.2.3"])
                .status()
                .expect("delete fixture tag");
            assert!(status.success());
        }
        match tag_kind {
            "missing" => {}
            "lightweight" => {
                assert!(git_at(&fixture.root)
                    .args(["tag", "v1.2.3"])
                    .status()
                    .expect("create lightweight tag")
                    .success());
            }
            "non-ancestor" => {
                let tree = git_at(&fixture.root)
                    .args(["rev-parse", "HEAD^{tree}"])
                    .output()
                    .expect("resolve fixture tree");
                assert!(tree.status.success());
                let tree = String::from_utf8(tree.stdout).expect("tree hash is UTF-8");
                let commit = git_at(&fixture.root)
                    .args(["commit-tree", tree.trim(), "-m", "unrelated release"])
                    .output()
                    .expect("create unrelated commit");
                assert!(commit.status.success());
                let commit = String::from_utf8(commit.stdout).expect("commit hash is UTF-8");
                assert!(git_at(&fixture.root)
                    .args(["tag", "-a", "v1.2.3", commit.trim(), "-m", "unrelated"])
                    .status()
                    .expect("create unrelated annotated tag")
                    .success());
            }
            "target-exists" => {
                assert!(git_at(&fixture.root)
                    .args(["tag", "-a", "v1.2.4", "-m", "already released"])
                    .status()
                    .expect("create target tag")
                    .success());
            }
            _ => unreachable!(),
        }

        let before = release_file_snapshot(&fixture);
        let output = fixture.run(&["--bump", "patch", "--date", RELEASE_DATE]);
        assert!(
            !output.status.success(),
            "{tag_kind} tag baseline unexpectedly passed"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        let expected = match tag_kind {
            "missing" => "is missing",
            "lightweight" => "must be annotated",
            "non-ancestor" => "ancestor of HEAD",
            "target-exists" => "already exists locally",
            _ => unreachable!(),
        };
        assert!(stderr.contains(expected), "unexpected diagnostic: {stderr}");
        assert_release_files_unchanged(&fixture, &before);
    }
}

#[test]
fn prepared_release_passes_the_real_document_checker() {
    let fixture = Fixture::new("1.2.3");
    let output = fixture.run_with_real_doc_checker(&["--bump", "patch", "--date", RELEASE_DATE]);
    assert!(
        output.status.success(),
        "integrated preparation failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn release_lockfile_awk_is_portable_and_gawk_lint_clean_in_every_mode() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("scripts/release-lockfile-packages.awk");
    let lockfile = root.join("Cargo.lock");
    let temp = tempfile::tempdir().expect("create AWK lint output directory");
    let awk_version = Command::new("awk").arg("--version").output();
    let is_gawk = awk_version.is_ok_and(|output| {
        String::from_utf8_lossy(&output.stdout).contains("GNU Awk")
            || String::from_utf8_lossy(&output.stderr).contains("GNU Awk")
    });

    for mode in ["list", "state", "rewrite"] {
        let mut command = Command::new("awk");
        if is_gawk {
            command.arg("--lint");
        }
        command.args(["-v", &format!("mode={mode}")]);
        if mode == "state" {
            command.args(["-v", "expected_version=0.5.2"]);
        } else if mode == "rewrite" {
            command.args([
                "-v",
                "next_version=0.5.3",
                "-v",
                &format!("count_file={}", temp.path().join("count").display()),
            ]);
        }
        let output = command
            .arg("-f")
            .arg(&script)
            .arg(&lockfile)
            .output()
            .unwrap_or_else(|error| panic!("run AWK lint in {mode} mode: {error}"));
        assert!(output.status.success(), "AWK {mode} mode failed");
        assert!(
            output.stderr.is_empty(),
            "AWK {mode} mode emitted lint diagnostics:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn release_metadata_preflight_rejects_a_future_stale_graph_missing_the_root_entry() {
    let fixture = Fixture::new("1.2.3");
    fs::create_dir_all(fixture.root.join("tools/future/src"))
        .expect("create future package source directory");
    write(
        &fixture.root.join("tools/future/Cargo.toml"),
        "[package]\nname = \"future\"\nversion = \"0.1.0\"\n",
    );
    write(&fixture.root.join("tools/future/src/lib.rs"), "");

    for manifest in [
        "Cargo.toml",
        "clients/native/Cargo.toml",
        "fuzz/Cargo.toml",
        "tools/future/Cargo.toml",
    ] {
        let generated = Command::new("cargo")
            .args(["generate-lockfile", "--manifest-path", manifest])
            .current_dir(&fixture.root)
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("generate realistic fixture lockfile");
        assert!(generated.status.success(), "generate {manifest}");
    }

    let future_manifest = read(fixture.root.join("tools/future/Cargo.toml"))
        + "\n[dependencies]\nsignal-fish-server = { path = \"../..\" }\n";
    write(
        &fixture.root.join("tools/future/Cargo.toml"),
        &future_manifest,
    );
    assert!(git_at(&fixture.root)
        .args([
            "add",
            "Cargo.lock",
            "clients/native/Cargo.lock",
            "fuzz/Cargo.lock",
            "tools/future",
        ])
        .status()
        .expect("stage stale graph fixture")
        .success());
    assert!(git_at(&fixture.root)
        .args([
            "commit",
            "--quiet",
            "-m",
            "add dependency without refreshing lock"
        ])
        .status()
        .expect("commit stale graph fixture")
        .success());

    let before = release_file_snapshot(&fixture);
    let output = fixture.run_with_actual_cargo(&["--bump", "patch", "--date", RELEASE_DATE]);
    assert!(
        !output.status.success(),
        "stale future graph unexpectedly passed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("lock file") && stderr.contains("needs to be updated"),
        "unexpected stale graph diagnostic:\n{}",
        stderr
    );
    assert_release_files_unchanged(&fixture, &before);
}

#[test]
fn prepared_release_resolver_handles_every_remote_state() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temp = tempfile::tempdir().expect("create resolver test directory");
    let fake_git = temp.path().join("git");
    write(
        &fake_git,
        r#"#!/usr/bin/env bash
set -euo pipefail
authenticated=false
if [ "${1:-}" = "-c" ]; then
    [[ "${2:-}" == http.https://github.com/.extraheader=AUTHORIZATION:* ]]
    authenticated=true
    shift 2
fi
case "${1:-}" in
    ls-remote)
        [ "$authenticated" = true ]
        if [[ "$*" == *"--tags"* ]]; then
            status=${TAG_STATUS:-2}
            [ "$status" -ne 0 ] || printf '%s\t%s\n' aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa refs/tags/v0.5.2
        else
            status=${BRANCH_STATUS:-2}
            [ "$status" -ne 0 ] || printf '%s\t%s\n' bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb refs/heads/release/v0.5.2
        fi
        [ "$status" -eq 0 ] || echo "remote probe failed" >&2
        exit "$status"
        ;;
    fetch)
        [ "$authenticated" = true ]
        ;;
    diff)
        exit "${TREE_STATUS:-0}"
        ;;
    *)
        echo "unexpected fake git invocation: $*" >&2
        exit 99
        ;;
esac
"#,
    );
    let mut permissions = fs::metadata(&fake_git)
        .expect("read fake git metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_git, permissions).expect("make fake git executable");
    let path = format!(
        "{}:{}",
        temp.path().display(),
        std::env::var("PATH").expect("PATH must be set")
    );
    let resolver = root.join("scripts/resolve-prepared-release.sh");

    for (description, tag_status, branch_status, tree_status, success, expected) in [
        ("absent branch", "2", "2", "0", true, "branch_exists=false"),
        ("matching branch", "2", "0", "0", true, "branch_exists=true"),
        ("conflicting tree", "2", "0", "1", false, "different tree"),
        ("tag exists", "0", "2", "0", false, "already exists"),
        (
            "branch API failure",
            "2",
            "128",
            "0",
            false,
            "Failed to check",
        ),
        (
            "tree comparison failure",
            "2",
            "0",
            "3",
            false,
            "Failed to compare",
        ),
    ] {
        let github_output = temp.path().join(format!("{description}.output"));
        let output = Command::new("bash")
            .arg(&resolver)
            .current_dir(root)
            .env("PATH", &path)
            .env("GH_TOKEN", "test-token")
            .env("GITHUB_OUTPUT", &github_output)
            .env("TAG_STATUS", tag_status)
            .env("BRANCH_STATUS", branch_status)
            .env("TREE_STATUS", tree_status)
            .output()
            .unwrap_or_else(|error| panic!("run {description} resolver case: {error}"));
        assert_eq!(output.status.success(), success, "{description}");
        let evidence = if success {
            read(&github_output)
        } else {
            String::from_utf8_lossy(&output.stderr).into_owned()
        };
        assert!(
            evidence.contains(expected),
            "{description} did not report {expected:?}:\n{evidence}"
        );
        if description == "matching branch" {
            assert!(evidence.contains("branch_sha=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"));
        }
    }
}

#[test]
fn release_pr_helper_reuses_creates_and_fails_closed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temp = tempfile::tempdir().expect("create PR helper test directory");
    let fake_gh = temp.path().join("gh");
    write(
        &fake_gh,
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$GH_LOG"
case "${1:-} ${2:-}" in
    "pr list")
        [ "${PR_LIST_STATUS:-0}" -eq 0 ] || exit "$PR_LIST_STATUS"
        if [[ "$*" == *"headRefOid"* ]]; then
            number=${PR_NUMBER:-${CREATED_NUMBER:-}}
            [ -z "$number" ] || printf '%s\t%s\n' "$number" "${PR_HEAD_SHA:-}"
        else
            printf '%s\n' "${PR_NUMBER:-}"
        fi
        ;;
    "pr create")
        echo "https://example.invalid/pull/1"
        ;;
    *) exit 99 ;;
esac
"#,
    );
    let mut permissions = fs::metadata(&fake_gh)
        .expect("read fake gh metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_gh, permissions).expect("make fake gh executable");
    let path = format!(
        "{}:{}",
        temp.path().display(),
        std::env::var("PATH").expect("PATH must be set")
    );
    let helper = root.join("scripts/ensure-release-pr.sh");
    let body = temp.path().join("body.md");
    write(&body, "release body\n");

    for (description, number, created, head_sha, list_status, success, creates) in [
        (
            "existing PR",
            "42",
            "",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "0",
            true,
            false,
        ),
        (
            "missing PR",
            "",
            "43",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "0",
            true,
            true,
        ),
        (
            "mismatched head",
            "42",
            "",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "0",
            false,
            false,
        ),
        ("PR API failure", "", "", "", "1", false, false),
    ] {
        let log = temp.path().join(format!("{description}.log"));
        let output = Command::new("bash")
            .arg(&helper)
            .args(["main", "release/v0.5.2", "0.5.2", "v0.5.2"])
            .arg(&body)
            .arg("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .env("PATH", &path)
            .env("GH_TOKEN", "test-token")
            .env("GITHUB_REPOSITORY", "owner/repo")
            .env("GH_LOG", &log)
            .env("PR_NUMBER", number)
            .env("CREATED_NUMBER", created)
            .env("PR_HEAD_SHA", head_sha)
            .env("PR_LIST_STATUS", list_status)
            .output()
            .unwrap_or_else(|error| panic!("run {description} helper case: {error}"));
        assert_eq!(output.status.success(), success, "{description}");
        let log = read(&log);
        assert!(log.contains("pr list"), "{description}");
        assert_eq!(log.contains("pr create"), creates, "{description}");
    }
}
