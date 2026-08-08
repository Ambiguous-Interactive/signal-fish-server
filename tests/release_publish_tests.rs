#![cfg(target_os = "linux")]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{repo_root, unique_temp_dir, write_file};

fn run(dir: &Path, program: &str, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap_or_else(|error| panic!("failed to run {program} {args:?}: {error}"))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = run(dir, "git", args);
    assert!(
        output.status.success(),
        "git {args:?} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output must be UTF-8")
        .trim()
        .to_owned()
}

fn write_release_metadata(root: &Path, version: &str) {
    write_file(
        &root.join("Cargo.toml"),
        &format!("[package]\nname = \"fixture\"\nversion = \"{version}\"\n"),
    );
    write_file(
        &root.join("CHANGELOG.md"),
        &format!("# Changelog\n\n## [Unreleased]\n\n## [{version}] - 2026-07-18\n"),
    );
}

struct ReleaseFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    output: PathBuf,
    resolver: PathBuf,
    reader: PathBuf,
}

impl ReleaseFixture {
    fn new(version: &str) -> Self {
        let temp = unique_temp_dir("release-source-resolver");
        let root = temp.path().join("work");
        let remote = temp.path().join("origin.git");
        fs::create_dir_all(&root).expect("create fixture worktree");
        assert!(run(temp.path(), "git", &["init", "--bare", "origin.git"])
            .status
            .success());
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.name", "Release Fixture"]);
        git(
            &root,
            &["config", "user.email", "release-fixture@example.com"],
        );
        // Never inherit workstation signing policy: these fixtures run without
        // an interactive agent and must not block waiting for a PIN or prompt.
        git(&root, &["config", "commit.gpgsign", "false"]);
        git(&root, &["config", "tag.gpgsign", "false"]);
        write_release_metadata(&root, version);
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "release source"]);
        git(
            &root,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        git(&root, &["push", "-u", "origin", "main"]);

        let project = repo_root();
        let output = temp.path().join("release-output");
        Self {
            output,
            resolver: project.join("scripts/resolve-release-source.sh"),
            reader: project.join("scripts/read-toml-string.sh"),
            _temp: temp,
            root,
        }
    }

    fn head(&self) -> String {
        git(&self.root, &["rev-parse", "HEAD^{commit}"])
    }

    fn commit(&self, message: &str) -> String {
        write_file(&self.root.join("workflow-marker"), message);
        git(&self.root, &["add", "workflow-marker"]);
        git(&self.root, &["commit", "-m", message]);
        git(&self.root, &["push", "origin", "main"]);
        self.head()
    }

    fn commit_version(&self, version: &str, message: &str) -> String {
        write_release_metadata(&self.root, version);
        git(&self.root, &["add", "Cargo.toml", "CHANGELOG.md"]);
        git(&self.root, &["commit", "-m", message]);
        git(&self.root, &["push", "origin", "main"]);
        self.head()
    }

    fn commit_version_with_changelog(
        &self,
        version: &str,
        changelog: &str,
        message: &str,
    ) -> String {
        write_file(
            &self.root.join("Cargo.toml"),
            &format!("[package]\nname = \"fixture\"\nversion = \"{version}\"\n"),
        );
        write_file(&self.root.join("CHANGELOG.md"), changelog);
        git(&self.root, &["add", "Cargo.toml", "CHANGELOG.md"]);
        git(&self.root, &["commit", "-m", message]);
        git(&self.root, &["push", "origin", "main"]);
        self.head()
    }

    fn commit_symlinked_version(&self, version: &str, message: &str) -> String {
        let manifest_target = "actual-Cargo.toml";
        write_file(
            &self.root.join(manifest_target),
            &format!("[package]\nname = \"fixture\"\nversion = \"{version}\"\n"),
        );
        fs::remove_file(self.root.join("Cargo.toml")).expect("remove fixture Cargo.toml");
        std::os::unix::fs::symlink(manifest_target, self.root.join("Cargo.toml"))
            .expect("symlink fixture Cargo.toml");
        git(&self.root, &["add", "Cargo.toml", manifest_target]);
        git(&self.root, &["commit", "-m", message]);
        git(&self.root, &["push", "origin", "main"]);
        assert_eq!(
            git(&self.root, &["status", "--porcelain=v1"]),
            "",
            "symlink fixture must represent a clean checkout"
        );
        assert_eq!(
            git(&self.root, &["show", "HEAD:Cargo.toml"]),
            manifest_target,
            "historical Cargo.toml lookup must read the symlink blob, not its target"
        );
        self.head()
    }

    fn commit_changelog(&self, changelog: &str, message: &str) -> String {
        write_file(&self.root.join("CHANGELOG.md"), changelog);
        git(&self.root, &["add", "CHANGELOG.md"]);
        git(&self.root, &["commit", "-m", message]);
        git(&self.root, &["push", "origin", "main"]);
        self.head()
    }

    fn commit_same_version_manifest_edit(&self, message: &str) -> String {
        let manifest =
            fs::read_to_string(self.root.join("Cargo.toml")).expect("read fixture Cargo.toml");
        write_file(
            &self.root.join("Cargo.toml"),
            &format!("{manifest}description = \"reviewed metadata edit\"\n"),
        );
        git(&self.root, &["add", "Cargo.toml"]);
        git(&self.root, &["commit", "-m", message]);
        git(&self.root, &["push", "origin", "main"]);
        self.head()
    }

    fn make_head_shallow(&self) {
        write_file(
            &self.root.join(".git/shallow"),
            &format!("{}\n", self.head()),
        );
    }

    fn tag(&self, annotated: bool, target: &str) {
        let mut args = vec!["tag"];
        if annotated {
            args.extend(["-a", "v1.2.3", target, "-m", "release"]);
        } else {
            args.extend(["v1.2.3", target]);
        }
        git(&self.root, &args);
        git(&self.root, &["push", "origin", "refs/tags/v1.2.3"]);
    }

    fn resolve(&self, event: &str, event_ref: &str) -> Output {
        let _ = fs::remove_file(&self.output);
        Command::new("bash")
            .arg(&self.resolver)
            .current_dir(&self.root)
            .env_remove("GIT_INDEX_FILE")
            .env("RELEASE_EVENT_NAME", event)
            .env("RELEASE_DEFAULT_BRANCH", "main")
            .env("RELEASE_EVENT_REF", event_ref)
            .env("RELEASE_OUTPUT_FILE", &self.output)
            .env("READ_TOML_SCRIPT", &self.reader)
            .output()
            .expect("run release resolver")
    }

    fn resolve_with_production_reader(&self, event: &str, event_ref: &str) -> Output {
        let _ = fs::remove_file(&self.output);
        Command::new("bash")
            .arg(&self.resolver)
            .current_dir(&self.root)
            .env_remove("GIT_INDEX_FILE")
            .env("RELEASE_EVENT_NAME", event)
            .env("RELEASE_DEFAULT_BRANCH", "main")
            .env("RELEASE_EVENT_REF", event_ref)
            .env("RELEASE_OUTPUT_FILE", &self.output)
            .output()
            .expect("run release resolver with production reader lookup")
    }

    fn resolved_value(&self, key: &str) -> String {
        fs::read_to_string(&self.output)
            .expect("read release outputs")
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("missing {key} output"))
            .to_owned()
    }
}

#[test]
fn release_source_resolver_covers_retry_and_rejection_states() {
    struct Case {
        name: &'static str,
        initial_version: &'static str,
        arrange: fn(&ReleaseFixture) -> String,
        event: &'static str,
        event_ref: &'static str,
        succeeds: bool,
        diagnostic: &'static str,
    }

    fn no_tag(fixture: &ReleaseFixture) -> String {
        fixture.head()
    }
    fn no_tag_after_later_commit(fixture: &ReleaseFixture) -> String {
        let release = fixture.head();
        fixture.commit("document prepared release");
        release
    }
    fn no_tag_after_same_version_manifest_edit(fixture: &ReleaseFixture) -> String {
        let release = fixture.head();
        fixture.commit_same_version_manifest_edit("edit package metadata without a version bump");
        release
    }
    fn repeated_version_boundary(fixture: &ReleaseFixture) -> String {
        let original = fixture.head();
        fixture.commit_version("1.2.4", "advance past candidate version");
        fixture.commit_version("1.2.3", "illegally reuse candidate version");
        original
    }
    fn incomplete_first_parent_history(fixture: &ReleaseFixture) -> String {
        let release = fixture.head();
        fixture.commit("later commit hidden from its parent history");
        fixture.make_head_shallow();
        release
    }
    fn no_version_introduction_match(fixture: &ReleaseFixture) -> String {
        fixture.commit_symlinked_version(
            "1.2.3",
            "make the working manifest version unavailable to historical reads",
        )
    }
    fn introduction_missing_changelog_section(fixture: &ReleaseFixture) -> String {
        let introduction = fixture.commit_version_with_changelog(
            "1.2.3",
            "# Changelog\n\n## [Unreleased]\n",
            "introduce version without release notes",
        );
        fixture.commit_changelog(
            "# Changelog\n\n## [Unreleased]\n\n## [1.2.3] - 2026-07-18\n",
            "add release notes too late",
        );
        introduction
    }
    fn introduction_mismatches_changelog_section(fixture: &ReleaseFixture) -> String {
        let introduction = fixture.commit_version_with_changelog(
            "1.2.3",
            "# Changelog\n\n## [Unreleased]\n\n## [1.2.4] - 2026-07-18\n",
            "introduce version with mismatched release notes",
        );
        fixture.commit_changelog(
            "# Changelog\n\n## [Unreleased]\n\n## [1.2.3] - 2026-07-18\n",
            "correct release notes too late",
        );
        introduction
    }
    fn matching_ancestor(fixture: &ReleaseFixture) -> String {
        let release = fixture.head();
        fixture.tag(true, &release);
        fixture.commit("workflow-only fix");
        release
    }
    fn lightweight(fixture: &ReleaseFixture) -> String {
        let release = fixture.head();
        fixture.tag(false, &release);
        release
    }
    fn non_ancestor(fixture: &ReleaseFixture) -> String {
        let tree = git(&fixture.root, &["rev-parse", "HEAD^{tree}"]);
        let commit = git(
            &fixture.root,
            &["commit-tree", &tree, "-m", "unrelated release"],
        );
        fixture.tag(true, &commit);
        commit
    }
    fn metadata_mismatch(fixture: &ReleaseFixture) -> String {
        write_release_metadata(&fixture.root, "1.2.2");
        git(&fixture.root, &["add", "Cargo.toml", "CHANGELOG.md"]);
        git(&fixture.root, &["commit", "-m", "mismatched source"]);
        let mismatched = fixture.head();
        fixture.tag(true, &mismatched);
        write_release_metadata(&fixture.root, "1.2.3");
        git(&fixture.root, &["add", "Cargo.toml", "CHANGELOG.md"]);
        git(&fixture.root, &["commit", "-m", "restore reviewed version"]);
        git(&fixture.root, &["push", "origin", "main"]);
        mismatched
    }
    fn direct_match(fixture: &ReleaseFixture) -> String {
        let release = fixture.head();
        fixture.tag(true, &release);
        release
    }
    fn direct_event_mismatch(fixture: &ReleaseFixture) -> String {
        let release = fixture.head();
        fixture.tag(true, &release);
        fixture.commit("later main commit")
    }

    let cases = [
        Case {
            name: "manual without tag uses version-introduction head",
            initial_version: "1.2.3",
            arrange: no_tag,
            event: "workflow_dispatch",
            event_ref: "refs/heads/main",
            succeeds: true,
            diagnostic: "",
        },
        Case {
            name: "manual without tag ignores a later documentation commit",
            initial_version: "1.2.3",
            arrange: no_tag_after_later_commit,
            event: "workflow_dispatch",
            event_ref: "refs/heads/main",
            succeeds: true,
            diagnostic: "",
        },
        Case {
            name: "manual without tag ignores a later same-version manifest edit",
            initial_version: "1.2.3",
            arrange: no_tag_after_same_version_manifest_edit,
            event: "workflow_dispatch",
            event_ref: "refs/heads/main",
            succeeds: true,
            diagnostic: "",
        },
        Case {
            name: "manual without tag rejects a reused version boundary",
            initial_version: "1.2.3",
            arrange: repeated_version_boundary,
            event: "workflow_dispatch",
            event_ref: "refs/heads/main",
            succeeds: false,
            diagnostic: "multiple first-parent introduction commits",
        },
        Case {
            name: "manual without tag rejects incomplete first-parent history",
            initial_version: "1.2.3",
            arrange: incomplete_first_parent_history,
            event: "workflow_dispatch",
            event_ref: "refs/heads/main",
            succeeds: false,
            diagnostic: "complete first-parent history",
        },
        Case {
            name: "manual without tag rejects zero version-introduction matches",
            initial_version: "1.2.2",
            arrange: no_version_introduction_match,
            event: "workflow_dispatch",
            event_ref: "refs/heads/main",
            succeeds: false,
            diagnostic: "No first-parent commit introduces release version 1.2.3",
        },
        Case {
            name: "manual without tag rejects an introduction missing changelog metadata",
            initial_version: "1.2.2",
            arrange: introduction_missing_changelog_section,
            event: "workflow_dispatch",
            event_ref: "refs/heads/main",
            succeeds: false,
            diagnostic: "CHANGELOG.md has no ## [1.2.3] release section",
        },
        Case {
            name: "manual without tag rejects mismatched introduction changelog metadata",
            initial_version: "1.2.2",
            arrange: introduction_mismatches_changelog_section,
            event: "workflow_dispatch",
            event_ref: "refs/heads/main",
            succeeds: false,
            diagnostic: "CHANGELOG.md has no ## [1.2.3] release section",
        },
        Case {
            name: "manual retry reuses matching annotated ancestor",
            initial_version: "1.2.3",
            arrange: matching_ancestor,
            event: "workflow_dispatch",
            event_ref: "refs/heads/main",
            succeeds: true,
            diagnostic: "",
        },
        Case {
            name: "lightweight tag is rejected",
            initial_version: "1.2.3",
            arrange: lightweight,
            event: "workflow_dispatch",
            event_ref: "refs/heads/main",
            succeeds: false,
            diagnostic: "lightweight",
        },
        Case {
            name: "unrelated tag is rejected",
            initial_version: "1.2.3",
            arrange: non_ancestor,
            event: "workflow_dispatch",
            event_ref: "refs/heads/main",
            succeeds: false,
            diagnostic: "not reachable",
        },
        Case {
            name: "tagged metadata mismatch is rejected",
            initial_version: "1.2.3",
            arrange: metadata_mismatch,
            event: "workflow_dispatch",
            event_ref: "refs/heads/main",
            succeeds: false,
            diagnostic: "source Cargo.toml",
        },
        Case {
            name: "direct matching annotated tag passes",
            initial_version: "1.2.3",
            arrange: direct_match,
            event: "push",
            event_ref: "refs/tags/v1.2.3",
            succeeds: true,
            diagnostic: "",
        },
        Case {
            name: "direct tag event commit mismatch is rejected",
            initial_version: "1.2.3",
            arrange: direct_event_mismatch,
            event: "push",
            event_ref: "refs/tags/v1.2.3",
            succeeds: false,
            diagnostic: "not release commit",
        },
    ];

    for case in cases {
        let fixture = ReleaseFixture::new(case.initial_version);
        let expected_source = (case.arrange)(&fixture);
        let output = fixture.resolve(case.event, case.event_ref);
        assert_eq!(
            output.status.success(),
            case.succeeds,
            "{}:\nstdout:\n{}\nstderr:\n{}",
            case.name,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if case.succeeds {
            assert_eq!(fixture.resolved_value("version"), "1.2.3", "{}", case.name);
            assert_eq!(
                fixture.resolved_value("source_revision"),
                expected_source,
                "{}",
                case.name
            );
        } else {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(case.diagnostic),
                "{} missing diagnostic `{}`:\n{}",
                case.name,
                case.diagnostic,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn release_source_resolver_preserves_dispatch_revision_tooling_across_detach() {
    let fixture = ReleaseFixture::new("1.2.3");
    let release = fixture.head();
    fixture.tag(true, &release);

    let dispatch_reader = fixture.root.join("scripts/read-toml-string.sh");
    fs::create_dir_all(dispatch_reader.parent().expect("reader parent"))
        .expect("create dispatch scripts directory");
    fs::copy(&fixture.reader, &dispatch_reader).expect("install dispatch revision reader");
    git(&fixture.root, &["add", "scripts/read-toml-string.sh"]);
    git(
        &fixture.root,
        &["commit", "-m", "add fixed release tooling"],
    );
    git(&fixture.root, &["push", "origin", "main"]);

    let output = fixture.resolve_with_production_reader("workflow_dispatch", "refs/heads/main");
    assert!(
        output.status.success(),
        "resolver must keep using dispatch-revision tooling after detaching to a tag that lacks it:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.resolved_value("source_revision"), release);
}

#[test]
fn clean_release_worktree_gate_detects_tracked_and_untracked_changes() {
    let cases = [
        ("clean", None, true, ""),
        (
            "tracked",
            Some(("tracked.txt", "changed")),
            false,
            "tracked.txt",
        ),
        (
            "untracked",
            Some(("crate-version.json", "{}")),
            false,
            "crate-version.json",
        ),
    ];
    let script = repo_root().join("scripts/check-clean-release-worktree.sh");

    for (name, mutation, succeeds, diagnostic) in cases {
        let root = unique_temp_dir(&format!("clean-release-{name}"));
        let root_path = root.path();
        git(root_path, &["init", "-b", "main"]);
        git(root_path, &["config", "user.name", "Release Fixture"]);
        git(
            root_path,
            &["config", "user.email", "release-fixture@example.com"],
        );
        git(root_path, &["config", "commit.gpgsign", "false"]);
        git(root_path, &["config", "tag.gpgsign", "false"]);
        write_file(&root_path.join("tracked.txt"), "original");
        git(root_path, &["add", "tracked.txt"]);
        git(root_path, &["commit", "-m", "baseline"]);
        if let Some((path, content)) = mutation {
            write_file(&root_path.join(path), content);
        }

        let output = run(root_path, "bash", &[script.to_str().expect("script path")]);
        assert_eq!(output.status.success(), succeeds, "{name}");
        if !succeeds {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(diagnostic),
                "{name} missing path diagnostic: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn crates_io_probe_is_idempotent_and_never_dirties_the_checkout() {
    struct Case {
        name: &'static str,
        status: &'static str,
        response_checksum: &'static str,
        published_revision: &'static str,
        published_dirty: &'static str,
        succeeds: bool,
        expected_output: &'static str,
        diagnostic: &'static str,
    }

    let expected_revision = "0123456789abcdef0123456789abcdef01234567";
    let cases = [
        Case {
            name: "absent version",
            status: "404",
            response_checksum: "actual",
            published_revision: expected_revision,
            published_dirty: "false",
            succeeds: true,
            expected_output: "exists=false",
            diagnostic: "",
        },
        Case {
            name: "matching published version",
            status: "200",
            response_checksum: "actual",
            published_revision: expected_revision,
            published_dirty: "false",
            succeeds: true,
            expected_output: "exists=true",
            diagnostic: "",
        },
        Case {
            name: "checksum mismatch",
            status: "200",
            response_checksum: "zeros",
            published_revision: expected_revision,
            published_dirty: "false",
            succeeds: false,
            expected_output: "",
            diagnostic: "does not match crates.io",
        },
        Case {
            name: "source revision mismatch",
            status: "200",
            response_checksum: "actual",
            published_revision: "89abcdef0123456789abcdef0123456789abcdef",
            published_dirty: "false",
            succeeds: false,
            expected_output: "",
            diagnostic: "already exists on crates.io from revision",
        },
        Case {
            name: "dirty published source",
            status: "200",
            response_checksum: "actual",
            published_revision: expected_revision,
            published_dirty: "true",
            succeeds: false,
            expected_output: "",
            diagnostic: "expected clean Cargo metadata",
        },
        Case {
            name: "clean cargo metadata omits dirty flag",
            status: "200",
            response_checksum: "actual",
            published_revision: expected_revision,
            published_dirty: "missing",
            succeeds: true,
            expected_output: "exists=true",
            diagnostic: "",
        },
        Case {
            name: "wrong type source cleanliness metadata",
            status: "200",
            response_checksum: "actual",
            published_revision: expected_revision,
            published_dirty: "string",
            succeeds: false,
            expected_output: "",
            diagnostic: "invalid-type",
        },
        Case {
            name: "registry outage",
            status: "503",
            response_checksum: "actual",
            published_revision: expected_revision,
            published_dirty: "false",
            succeeds: false,
            expected_output: "",
            diagnostic: "failed closed with HTTP 503",
        },
    ];

    for case in cases {
        let root = unique_temp_dir(&format!("crates-probe-{}", case.name.replace(' ', "-")));
        let root_path = root.path();
        let bin = root_path.join("bin");
        let package = root_path.join("package/signal-fish-server-1.2.3");
        let runner_temp = root_path.join("runner-temp");
        let checkout = root_path.join("checkout");
        fs::create_dir_all(&bin).expect("create mock bin");
        fs::create_dir_all(&package).expect("create package fixture");
        fs::create_dir_all(&runner_temp).expect("create runner temp");
        fs::create_dir_all(&checkout).expect("create clean checkout");
        git(&checkout, &["init", "-b", "main"]);
        git(&checkout, &["config", "user.name", "Release Fixture"]);
        git(
            &checkout,
            &["config", "user.email", "release-fixture@example.com"],
        );
        git(&checkout, &["config", "commit.gpgsign", "false"]);
        write_file(&checkout.join("tracked.txt"), "baseline");
        git(&checkout, &["add", "tracked.txt"]);
        git(&checkout, &["commit", "-m", "baseline"]);
        let dirty_metadata = match case.published_dirty {
            "false" | "true" => format!(",\"dirty\":{}", case.published_dirty),
            "missing" => String::new(),
            "string" => ",\"dirty\":\"false\"".to_owned(),
            value => panic!("unknown dirty metadata fixture {value}"),
        };
        write_file(
            &package.join(".cargo_vcs_info.json"),
            &format!(
                "{{\"git\":{{\"sha1\":\"{}\"{dirty_metadata}}}}}",
                case.published_revision
            ),
        );
        let crate_file = root_path.join("signal-fish-server-1.2.3.crate");
        assert!(run(
            root_path,
            "tar",
            &[
                "-czf",
                crate_file.to_str().expect("crate path"),
                "-C",
                root_path.join("package").to_str().expect("package path"),
                "signal-fish-server-1.2.3",
            ],
        )
        .status
        .success());
        let actual_checksum = git_style_sha256(&crate_file);
        let response_checksum = match case.response_checksum {
            "actual" => actual_checksum,
            "zeros" => "0".repeat(64),
            _ => unreachable!(),
        };
        let response = root_path.join("response.json");
        write_file(
            &response,
            &format!("{{\"version\":{{\"checksum\":\"{response_checksum}\"}}}}"),
        );
        let curl = bin.join("curl");
        write_file(
            &curl,
            "#!/usr/bin/env bash\nset -euo pipefail\noutput=\"\"\nurl=\"\"\nwhile (($#)); do\n  case \"$1\" in\n    --output) output=$2; shift 2 ;;\n    http*) url=$1; shift ;;\n    *) shift ;;\n  esac\ndone\nif [[ \"$url\" == */download ]]; then\n  cp \"$MOCK_CRATE\" \"$output\"\nelse\n  cp \"$MOCK_RESPONSE\" \"$output\"\n  printf '%s' \"$MOCK_STATUS\"\nfi\n",
        );
        let mut permissions = fs::metadata(&curl).expect("curl metadata").permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o755);
        }
        fs::set_permissions(&curl, permissions).expect("make mock curl executable");

        let output_file = root_path.join("outputs");
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = Command::new("bash")
            .arg(repo_root().join("scripts/check-crates-io-release.sh"))
            .current_dir(&checkout)
            .env_remove("GIT_INDEX_FILE")
            .env("PATH", path)
            .env("MOCK_CRATE", &crate_file)
            .env("MOCK_RESPONSE", &response)
            .env("MOCK_STATUS", case.status)
            .env("RUNNER_TEMP", &runner_temp)
            .env("RELEASE_VERSION", "1.2.3")
            .env("RELEASE_SOURCE_REVISION", expected_revision)
            .env("RELEASE_OUTPUT_FILE", &output_file)
            .output()
            .expect("run crates.io probe");
        assert_eq!(
            output.status.success(),
            case.succeeds,
            "{}:\nstdout:\n{}\nstderr:\n{}",
            case.name,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if case.succeeds {
            assert_eq!(
                fs::read_to_string(&output_file)
                    .expect("probe output")
                    .trim(),
                case.expected_output,
                "{}",
                case.name
            );
        } else {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(case.diagnostic),
                "{} missing diagnostic `{}`: {}",
                case.name,
                case.diagnostic,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(
            fs::read_dir(&runner_temp)
                .expect("read runner temp")
                .count(),
            0,
            "{} left registry scratch files behind",
            case.name
        );
        assert_eq!(
            git(
                &checkout,
                &["status", "--porcelain=v1", "--untracked-files=all"]
            ),
            "",
            "{} dirtied the release checkout",
            case.name
        );
    }
}

#[test]
fn github_release_probe_validates_public_identity_before_asset_mutation() {
    struct Case {
        name: &'static str,
        status: &'static str,
        mutation: &'static str,
        succeeds: bool,
        expected_output: &'static str,
        diagnostic: &'static str,
    }

    let cases = [
        Case {
            name: "absent release",
            status: "404",
            mutation: "none",
            succeeds: true,
            expected_output: "exists=false",
            diagnostic: "",
        },
        Case {
            name: "matching public release",
            status: "200",
            mutation: "none",
            succeeds: true,
            expected_output: "exists=true",
            diagnostic: "",
        },
        Case {
            name: "matching public release by notes digest",
            status: "200",
            mutation: "hash",
            succeeds: true,
            expected_output: "exists=true",
            diagnostic: "",
        },
        Case {
            name: "required release is absent",
            status: "404",
            mutation: "require-existing",
            succeeds: false,
            expected_output: "",
            diagnostic: "does not exist before asset upload",
        },
        Case {
            name: "tag mismatch",
            status: "200",
            mutation: "tag",
            succeeds: false,
            expected_output: "",
            diagnostic: "tag_name",
        },
        Case {
            name: "name mismatch",
            status: "200",
            mutation: "name",
            succeeds: false,
            expected_output: "",
            diagnostic: "name",
        },
        Case {
            name: "draft release",
            status: "200",
            mutation: "draft",
            succeeds: false,
            expected_output: "",
            diagnostic: "draft",
        },
        Case {
            name: "prerelease",
            status: "200",
            mutation: "prerelease",
            succeeds: false,
            expected_output: "",
            diagnostic: "prerelease",
        },
        Case {
            name: "missing source provenance",
            status: "200",
            mutation: "source",
            succeeds: false,
            expected_output: "",
            diagnostic: "source revision note",
        },
        Case {
            name: "missing image provenance",
            status: "200",
            mutation: "digest",
            succeeds: false,
            expected_output: "",
            diagnostic: "image digest note",
        },
        Case {
            name: "stale notes with provenance tokens",
            status: "200",
            mutation: "body",
            succeeds: false,
            expected_output: "",
            diagnostic: "release notes body",
        },
        Case {
            name: "trailing newline drift",
            status: "200",
            mutation: "trailing-newline",
            succeeds: false,
            expected_output: "",
            diagnostic: "release notes body",
        },
        Case {
            name: "malformed response",
            status: "200",
            mutation: "malformed",
            succeeds: false,
            expected_output: "",
            diagnostic: "malformed metadata",
        },
        Case {
            name: "api outage",
            status: "503",
            mutation: "none",
            succeeds: false,
            expected_output: "",
            diagnostic: "HTTP 503",
        },
    ];

    let source_revision = "0123456789abcdef0123456789abcdef01234567";
    let image_digest = format!("sha256:{}", "a".repeat(64));
    let expected_body = format!(
        "Release notes\n\nMulti-architecture manifest digest: `{image_digest}`\n\nSource revision: `{source_revision}`\n"
    );

    for case in cases {
        let root = unique_temp_dir(&format!("github-release-{}", case.name.replace(' ', "-")));
        let bin = root.path().join("bin");
        fs::create_dir_all(&bin).expect("create mock bin");
        let notes = root.path().join("release-notes.md");
        write_file(&notes, &expected_body);

        let mut response = serde_json::json!({
            "tag_name": "v1.2.3",
            "name": "v1.2.3",
            "draft": false,
            "prerelease": false,
            "body": expected_body,
        });
        match case.mutation {
            "none" | "hash" | "require-existing" | "malformed" => {}
            "tag" => response["tag_name"] = serde_json::json!("v1.2.2"),
            "name" => response["name"] = serde_json::json!("wrong release"),
            "draft" => response["draft"] = serde_json::json!(true),
            "prerelease" => response["prerelease"] = serde_json::json!(true),
            "source" => response["body"] = serde_json::json!(format!(
                "Release notes\n\nMulti-architecture manifest digest: `{image_digest}`\n"
            )),
            "digest" => response["body"] = serde_json::json!(format!(
                "Release notes\n\nSource revision: `{source_revision}`\n"
            )),
            "body" => response["body"] = serde_json::json!(format!(
                "Stale notes\n\nMulti-architecture manifest digest: `{image_digest}`\n\nSource revision: `{source_revision}`\n"
            )),
            "trailing-newline" => {
                response["body"] = serde_json::json!(format!("{expected_body}\n"));
            }
            mutation => panic!("unknown release mutation {mutation}"),
        }
        let response_file = root.path().join("response.json");
        if case.mutation == "malformed" {
            write_file(&response_file, "not-json");
        } else {
            write_file(&response_file, &response.to_string());
        }

        let gh = bin.join("gh");
        write_file(
            &gh,
            "#!/usr/bin/env bash\nset -euo pipefail\nif [[ \"$*\" == *\"--include\"* ]]; then\n  printf 'HTTP/2 %s\\n' \"$MOCK_STATUS\"\n  exit 1\nfi\nif [ \"$MOCK_STATUS\" = 200 ]; then\n  cat \"$MOCK_RESPONSE\"\n  exit 0\nfi\nexit 1\n",
        );
        let mut permissions = fs::metadata(&gh).expect("gh metadata").permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o755);
        }
        fs::set_permissions(&gh, permissions).expect("make mock gh executable");

        let output_file = root.path().join("outputs");
        let expected_notes_sha256 = git_style_sha256(&notes);
        let mut command = Command::new("bash");
        command
            .arg(repo_root().join("scripts/check-github-release.sh"))
            .env("GH_CLI", &gh)
            .env("MOCK_STATUS", case.status)
            .env("MOCK_RESPONSE", &response_file)
            .env(
                "GITHUB_REPOSITORY",
                "Ambiguous-Interactive/signal-fish-server",
            )
            .env("TAG", "v1.2.3")
            .env("RELEASE_NAME", "v1.2.3")
            .env("RELEASE_SOURCE_REVISION", source_revision)
            .env("RELEASE_IMAGE_DIGEST", &image_digest)
            .env("RELEASE_OUTPUT_FILE", &output_file);
        if case.mutation == "hash" {
            command.env("RELEASE_NOTES_SHA256", expected_notes_sha256);
        } else {
            command.env("RELEASE_NOTES_FILE", &notes);
        }
        if case.mutation == "require-existing" {
            command.env("RELEASE_REQUIRE_EXISTING", "true");
        }
        let output = command.output().expect("run GitHub Release probe");
        assert_eq!(
            output.status.success(),
            case.succeeds,
            "{}:\nstdout:\n{}\nstderr:\n{}",
            case.name,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if case.succeeds {
            assert_eq!(
                fs::read_to_string(&output_file)
                    .expect("probe output")
                    .trim(),
                case.expected_output,
                "{}",
                case.name
            );
        } else {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(case.diagnostic),
                "{} missing diagnostic `{}`: {}",
                case.name,
                case.diagnostic,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

fn git_style_sha256(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("run sha256sum");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("sha256sum output")
        .split_whitespace()
        .next()
        .expect("checksum")
        .to_owned()
}
