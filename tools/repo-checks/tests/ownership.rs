//! Keep test ownership complete without linking the server into repository checks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn metadata(manifest: &Path) -> serde_json::Value {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(manifest)
        .output()
        .expect("read Cargo target inventory");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("Cargo metadata JSON")
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tools")
        .parent()
        .expect("repository")
        .to_path_buf()
}

#[test]
fn every_root_test_source_has_exactly_one_cargo_owner() {
    let root = root();
    let mut owners: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for manifest in [
        root.join("Cargo.toml"),
        root.join("tools/repo-checks/Cargo.toml"),
    ] {
        let data = metadata(&manifest);
        for package in data["packages"].as_array().expect("packages") {
            for target in package["targets"].as_array().expect("targets") {
                if target["kind"] == serde_json::json!(["test"]) {
                    let source = Path::new(target["src_path"].as_str().expect("source path"))
                        .canonicalize()
                        .expect("test source exists");
                    owners
                        .entry(source)
                        .or_default()
                        .push(package["name"].as_str().expect("package name").to_owned());
                }
            }
        }
    }
    for directory in [root.join("tests"), root.join("tools/repo-checks/tests")] {
        for entry in std::fs::read_dir(directory).expect("test sources") {
            let source = entry.expect("directory entry").path();
            if source
                .extension()
                .is_some_and(|extension| extension == "rs")
            {
                let source = source.canonicalize().expect("source exists");
                assert_eq!(
                    owners.get(&source).map(Vec::len),
                    Some(1),
                    "{} must have exactly one Cargo test owner",
                    source.display()
                );
            }
        }
    }
}

#[test]
fn tooling_has_no_server_or_path_runtime_dependency() {
    let data = metadata(&root().join("tools/repo-checks/Cargo.toml"));
    for dependency in data["packages"][0]["dependencies"]
        .as_array()
        .expect("dependencies")
    {
        assert_ne!(dependency["name"], "signal-fish-server");
        assert!(
            dependency["path"].is_null(),
            "repository checks must not depend on runtime path crates: {dependency}"
        );
    }
}
