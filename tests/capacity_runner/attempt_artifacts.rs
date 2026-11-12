//! Preserve replacement attempts after success, invalidation, and test unwinding (#1352).
use std::path::{Path, PathBuf};

pub(crate) struct AttemptDirectory {
    path: PathBuf,
}

impl AttemptDirectory {
    pub(crate) fn replacement() -> Self {
        let mut root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/capacity-attempts");
        if let (Ok(run), Ok(attempt)) = (
            std::env::var("GITHUB_RUN_ID"),
            std::env::var("GITHUB_RUN_ATTEMPT"),
        ) {
            root = root.join(run).join(attempt);
        }
        let attempt = Self::new_in(&root);
        let executable = std::env::current_exe().expect("test executable path");
        let (sha256, bytes) = super::diagnostics::sha256_file(&executable)
            .expect("hash the running replacement test executable");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .expect("read replacement source identity");
            assert!(output.status.success(), "git identity failed: {output:?}");
            output.stdout
        };
        let revision = git(&["rev-parse", "HEAD"]);
        let patch = git(&["diff", "HEAD", "--binary"]);
        std::fs::write(attempt.path.join("source.patch"), &patch).expect("retain source patch");
        let identity = serde_json::json!({
            "git_head": String::from_utf8(revision).expect("git revision is UTF-8").trim(),
            "tracked_patch_sha256": super::diagnostics::sha256_bytes(&patch),
            "test_executable_sha256": sha256,
            "test_executable_bytes": bytes,
            "llvm_profile_file": std::env::var_os("LLVM_PROFILE_FILE"),
            "rustflags": std::env::var_os("RUSTFLAGS"),
            "encoded_rustflags": std::env::var_os("CARGO_ENCODED_RUSTFLAGS"),
            "github_run_id": std::env::var_os("GITHUB_RUN_ID"),
            "github_run_attempt": std::env::var_os("GITHUB_RUN_ATTEMPT"),
        });
        super::artifacts::write_json(attempt.path.join("attempt-identity.json"), &identity)
            .expect("retain replacement attempt identity");
        eprintln!("Room replacement attempt: {}", attempt.path.display());
        attempt
    }

    fn new_in(root: &Path) -> Self {
        std::fs::create_dir_all(root).expect("create capacity attempt archive");
        let path = tempfile::Builder::new()
            .prefix("room-replacement-")
            .tempdir_in(root)
            .expect("create separate replacement attempt")
            .keep();
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for AttemptDirectory {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        // Keep the last peer stages in the job log even if artifact upload fails.
        for name in [
            "attempt-identity.json",
            "peer-progress.json",
            "summary.json",
        ] {
            let file = std::fs::File::open(self.path.join(name));
            let mut bytes = Vec::new();
            match file.and_then(|file| {
                use std::io::Read;
                file.take(64 * 1024).read_to_end(&mut bytes)
            }) {
                Ok(_) => eprintln!("{name}: {}", String::from_utf8_lossy(&bytes)),
                Err(error) => eprintln!("{}: {error}", self.path.join(name).display()),
            }
        }
        eprintln!(
            "Retained failed replacement attempt: {}",
            self.path.display()
        );
    }
}

#[test]
fn replacement_attempt_survives_unwinding_without_overwriting_other_attempts() {
    let root = tempfile::tempdir().unwrap();
    let first = AttemptDirectory::new_in(root.path());
    std::fs::write(
        first.path().join("peer-progress.json"),
        br#"{"held":"join_snapshot"}"#,
    )
    .unwrap();
    let first_path = first.path().to_owned();
    let second_path = std::panic::catch_unwind(|| {
        let second = AttemptDirectory::new_in(root.path());
        std::fs::write(
            second.path().join("peer-progress.json"),
            br#"{"held":"socket_connect"}"#,
        )
        .unwrap();
        panic!("controlled replacement failure");
    });
    assert!(second_path.is_err());
    drop(first);
    let paths: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(paths.len(), 2, "both attempts must survive wrapper drop");
    assert_eq!(
        std::fs::read(first_path.join("peer-progress.json")).unwrap(),
        br#"{"held":"join_snapshot"}"#
    );
    let second = paths.iter().find(|path| **path != first_path).unwrap();
    assert_eq!(
        std::fs::read(second.join("peer-progress.json")).unwrap(),
        br#"{"held":"socket_connect"}"#
    );
}
