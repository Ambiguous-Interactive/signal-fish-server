//! Behavioral tests for the shared skip-if-verified composite action
//! (`.github/actions/skip-if-verified`).
//!
//! The guard is fail-open CI infrastructure: its failure modes are silent by
//! design, so a YAML pin can only see its wiring, never its runtime behavior.
//! These tests extract the action's `run:` block and execute it under bash
//! with a canned `gh` shim, pinning the whole contract: exit 0 on every path,
//! `duplicate` decisions, anchor semantics, freshness-window semantics, and
//! fail-open behavior on malformed payloads and bad inputs (Bugbot findings,
//! PR #704).

#[cfg(unix)]
mod common;

#[cfg(unix)]
use common::{bash_command, read_live_file, repo_root, unique_temp_dir, write_file};

#[cfg(unix)]
/// Extract the action's single composite step's `run: |` block, dedented.
fn action_run_block() -> String {
    let action = read_live_file(&repo_root().join(".github/actions/skip-if-verified/action.yml"));
    let marker = "      run: |";
    let start = action
        .find(marker)
        .expect("action.yml must declare a `run: |` block on its guard step");
    let body = &action[start + marker.len()..];
    let mut block = String::new();
    for line in body.lines() {
        if line.is_empty() {
            block.push('\n');
            continue;
        }
        let Some(content) = line.strip_prefix("        ") else {
            break; // dedented past the block: the step ended
        };
        block.push_str(content);
        block.push('\n');
    }
    assert!(
        block.contains("duplicate="),
        "extracted run block must be the guard script (got: {block:?})"
    );
    // The guard runs on every runner family its callers use, and the
    // harness runs on the macOS cron lane: GNU-only constructs (`date -d`)
    // break under BSD date. The window must stay portable arithmetic over
    // `date +%s` (Bugbot finding, PR #704 round 2).
    assert!(
        !block.contains("date -d") && !block.contains("date -j"),
        "the guard script must not use GNU-only `date -d`/`date -j` parsing; \
         compute the cutoff from `date +%s` with shell arithmetic instead"
    );
    block
}

#[cfg(unix)]
fn make_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .expect("file metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("make file executable");
}

#[cfg(unix)]
/// Canned `gh` shim. `gh api` passes the endpoint as `$2` (`api` is `$1`).
/// Modes and payloads come from the environment so the script stays a plain
/// raw string (no format! brace-escaping against shell syntax). Timestamps
/// come from `jq` (a dependency of the guard itself), not GNU `date -d`, so
/// the harness behaves identically on Linux and the BSD-date macOS cron lane.
const GH_SHIM: &str = r#"#!/usr/bin/env bash
set -u
url="${2:-}"
case "$url" in
  *"actions/runs?"*)
    if [ "${MOCK_RUNS_FAIL:-0}" = "1" ]; then echo "simulated runs API failure" >&2; exit 70; fi
    recent="$(jq -rn 'now - 3600 | todateiso8601')"
    old="$(jq -rn 'now - 720000 | todateiso8601')"
    case "${MOCK_RUNS_MODE:-}" in
      recent|old)
        stamp="$recent"; [ "${MOCK_RUNS_MODE}" = "old" ] && stamp="$old"
        printf '{"total_count":1,"workflow_runs":[{"id":999,"path":"%s","status":"completed","conclusion":"success","created_at":"%s","html_url":"https://example.test/runs/999"}]}' \
          "${MOCK_WORKFLOW_PATH}" "$stamp" ;;
      wrong_path)
        printf '{"total_count":1,"workflow_runs":[{"id":999,"path":".github/workflows/other.yml","status":"completed","conclusion":"success","created_at":"%s","html_url":"https://example.test/runs/999"}]}' "$recent" ;;
      empty) printf '{"total_count":0,"workflow_runs":[]}' ;;
      invalid) printf 'not-json-at-all' ;;
      *) echo "fake gh: unexpected runs mode ${MOCK_RUNS_MODE:-}" >&2; exit 90 ;;
    esac ;;
  *"actions/runs/999/jobs"*|*"jobs?"*)
    if [ "${MOCK_JOBS_FAIL:-0}" = "1" ]; then echo "simulated jobs API failure" >&2; exit 71; fi
    case "${MOCK_JOBS_MODE:-}" in
      anchored)
        printf '{"jobs":[{"id":1,"name":"job","status":"completed","conclusion":"success","steps":[{"name":"%s","conclusion":"success"},{"name":"other step","conclusion":"success"}]}]}' \
          "${MOCK_ANCHOR_STEP}" ;;
      no_anchor)
        printf '{"jobs":[{"id":1,"name":"job","status":"completed","conclusion":"success","steps":[{"name":"Checkout repository","conclusion":"success"}]}]}' ;;
      invalid) printf '{{{' ;;
      *) echo "fake gh: unexpected jobs mode ${MOCK_JOBS_MODE:-}" >&2; exit 90 ;;
    esac ;;
  *) echo "fake gh: unexpected endpoint: $url" >&2; exit 90 ;;
esac
"#;

#[cfg(unix)]
/// One behavioral case for the guard script.
struct Case {
    name: &'static str,
    runs_mode: &'static str,
    jobs_mode: &'static str,
    runs_fail: bool,
    jobs_fail: bool,
    gh_token: Option<&'static str>,
    /// The non-default `GITHUB_WORKFLOW_REF` spelling under test.
    workflow_ref: &'static str,
    max_age_hours: &'static str,
    expected_duplicate: bool,
    /// Only genuine failure paths warn; "no match, run normally" must not.
    expected_warning: bool,
}

#[cfg(unix)]
impl Case {
    fn new(
        name: &'static str,
        runs_mode: &'static str,
        jobs_mode: &'static str,
        expected_duplicate: bool,
    ) -> Self {
        Self {
            name,
            runs_mode,
            jobs_mode,
            runs_fail: false,
            jobs_fail: false,
            gh_token: Some("test-token"),
            workflow_ref: "Acme/signal-fish-server/.github/workflows/ci-safety.yml@refs/heads/main",
            max_age_hours: "168",
            expected_duplicate,
            expected_warning: false,
        }
    }

    fn warns(mut self) -> Self {
        self.expected_warning = true;
        self
    }
}

#[cfg(unix)]
#[test]
fn skip_if_verified_action_contract() {
    // Data-driven, red-green harness. Every case must exit 0 (the action's
    // never-fail contract); the `duplicate` output carries the decision.
    let cases = [
        // A prior successful run whose jobs show the executed anchor counts
        // as verification.
        Case::new("anchored_prior_success", "recent", "anchored", true),
        // A prior success WITHOUT an executed anchor step (a run this guard
        // itself skipped) must never count — otherwise one skip sustains
        // itself forever and the window never re-arms (PR #703 round 2).
        Case::new("skipped_run_never_verifies", "recent", "no_anchor", false),
        // Older than the freshness window: re-run.
        Case::new("expired_window", "old", "anchored", false),
        // Contract: max_success_age_hours=0 DISABLES the window, so even an
        // old anchored success counts.
        Case {
            max_age_hours: "0",
            ..Case::new("zero_disables_window", "old", "anchored", true)
        },
        // An unreadable window value must fail OPEN: nothing may match
        // (the fallback cutoff is "now", never "0" — epoch would count every
        // prior success and bias the guard toward skipping).
        Case {
            max_age_hours: "soon",
            ..Case::new("garbage_window_fails_open", "old", "anchored", false).warns()
        },
        // Malformed payloads must fail OPEN (duplicate=false, exit 0), never
        // abort the step: an in-job caller (unused-deps) would otherwise fail
        // the job and skip the analyzers the guard protects.
        Case::new(
            "invalid_runs_payload_fails_open",
            "invalid",
            "anchored",
            false,
        )
        .warns(),
        Case::new(
            "invalid_jobs_payload_fails_open",
            "recent",
            "invalid",
            false,
        )
        .warns(),
        // Empty run list: nothing verified this head; run normally.
        Case::new("empty_runs", "empty", "anchored", false),
        // A different workflow's runs never verify this workflow.
        Case::new("different_workflow_path", "wrong_path", "anchored", false),
        // API failures fail open with the permission-naming warning.
        Case {
            runs_fail: true,
            ..Case::new("runs_api_failure", "recent", "anchored", false).warns()
        },
        Case {
            jobs_fail: true,
            ..Case::new("jobs_api_failure", "recent", "anchored", false).warns()
        },
        // Missing token: fail open (running).
        Case {
            gh_token: None,
            ..Case::new("no_token_fails_open", "recent", "anchored", false).warns()
        },
        // The repository-relative GITHUB_WORKFLOW_REF spelling must normalize
        // to the same decision as the owner-qualified one.
        Case {
            workflow_ref: ".github/workflows/ci-safety.yml@refs/heads/main",
            ..Case::new("repo_relative_ref_spelling", "recent", "anchored", true)
        },
    ];

    let anchor = "Run Miri on library tests";
    let fixed_env = [
        ("GITHUB_SHA", "1111111111111111111111111111111111111111"),
        ("GITHUB_RUN_ID", "12345"),
        ("GITHUB_REPOSITORY", "Acme/signal-fish-server"),
        ("ANCHOR_STEP", anchor),
    ];

    for case in &cases {
        let fixture = unique_temp_dir(&format!("skip-if-verified-{}", case.name));
        let fake_bin = fixture.path().join("bin");
        std::fs::create_dir_all(&fake_bin).expect("create fake bin dir");

        let shim = fixture.path().join("bin").join("gh");
        write_file(&shim, GH_SHIM);
        make_executable(&shim);

        let script_path = fixture.path().join("guard.sh");
        write_file(&script_path, &action_run_block());
        make_executable(&script_path);
        let output_file = fixture.path().join("github-output.txt");
        let summary_file = fixture.path().join("step-summary.md");

        let mut command = bash_command();
        command
            .arg(&script_path)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    fake_bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("GITHUB_OUTPUT", &output_file)
            .env("GITHUB_STEP_SUMMARY", &summary_file)
            .env("MOCK_RUNS_MODE", case.runs_mode)
            .env("MOCK_JOBS_MODE", case.jobs_mode)
            .env("MOCK_RUNS_FAIL", if case.runs_fail { "1" } else { "0" })
            .env("MOCK_JOBS_FAIL", if case.jobs_fail { "1" } else { "0" })
            .env("MOCK_WORKFLOW_PATH", ".github/workflows/ci-safety.yml")
            .env("MOCK_ANCHOR_STEP", anchor)
            .env("MAX_SUCCESS_AGE_HOURS", case.max_age_hours)
            .env("GITHUB_WORKFLOW_REF", case.workflow_ref);
        for (key, value) in fixed_env {
            command.env(key, value);
        }
        match case.gh_token {
            Some(token) => {
                command.env("GH_TOKEN", token);
            }
            None => {
                command.env_remove("GH_TOKEN");
            }
        }

        let output = command.output().unwrap_or_else(|panic| {
            panic!("case {}: failed to run guard script: {panic}", case.name)
        });
        // GitHub Actions workflow commands (`::warning::`) are parsed from
        // stdout, so warnings ride stdout; capture both for diagnostics.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let outputs = std::fs::read_to_string(&output_file).unwrap_or_default();
        let duplicate = outputs
            .lines()
            .find_map(|line| line.strip_prefix("duplicate="))
            .unwrap_or("<missing>");

        assert_eq!(
            output.status.code(),
            Some(0),
            "case {}: the guard must ALWAYS exit 0 (fail-open contract).\nstdout:\n{stdout}\nstderr:\n{stderr}",
            case.name
        );
        assert_eq!(
            duplicate,
            if case.expected_duplicate {
                "true"
            } else {
                "false"
            },
            "case {}: unexpected duplicate decision.\nstdout:\n{stdout}\nstderr:\n{stderr}",
            case.name
        );
        assert_eq!(
            stdout.contains("::warning::"),
            case.expected_warning,
            "case {}: unexpected warning-channel usage.\nstdout:\n{stdout}\nstderr:\n{stderr}",
            case.name
        );
        if case.expected_duplicate {
            assert!(
                stdout.contains("already verified") && summary_file.exists(),
                "case {}: a skip must emit the notice and the step summary.\nstdout:\n{stdout}",
                case.name
            );
        }
    }
}
