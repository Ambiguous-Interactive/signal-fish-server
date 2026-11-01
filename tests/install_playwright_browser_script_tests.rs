//! Exercise the browser installer through its real CLI and package-relative lookup.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};

/// Keep Node optional for local Rust-only builds, but require it in every CI run.
fn node_available() -> bool {
    match Command::new("node").arg("--version").output() {
        Ok(output) => {
            assert!(
                output.status.success(),
                "Node version probe failed: {output:?}"
            );
            true
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && std::env::var_os("CI").is_none()
                && std::env::var_os("GITHUB_ACTIONS").is_none() =>
        {
            eprintln!("SKIP browser installer fixture: Node.js is not installed for this local Rust-only build");
            false
        }
        Err(error) => panic!(
            "Cannot run Node.js for browser installer fixtures; CI requires Node.js: {error}"
        ),
    }
}

struct BrowserFixture {
    root: tempfile::TempDir,
    script: PathBuf,
    events: PathBuf,
}

impl BrowserFixture {
    /// Copy the real entrypoint beside a local package that models browser readiness.
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("playwright browser fixture ")
            .tempdir()
            .expect("create browser fixture");
        let scripts = root.path().join("scripts");
        let package = root
            .path()
            .join("clients/browser/node_modules/playwright-core");
        fs::create_dir_all(&scripts).unwrap();
        fs::create_dir_all(&package).unwrap();
        fs::create_dir_all(root.path().join("elsewhere")).unwrap();
        fs::write(root.path().join("clients/browser/package.json"), "{}").unwrap();
        let script = scripts.join("install-playwright-browser.mjs");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install-playwright-browser.mjs"),
            &script,
        )
        .expect("copy actual browser installer");
        fs::write(
            package.join("package.json"),
            r#"{"name":"playwright-core","type":"commonjs","main":"index.cjs","exports":{".":"./index.cjs","./package.json":"./package.json"}}"#,
        )
        .unwrap();
        fs::write(
            package.join("cli.js"),
            r#"const fs = require('node:fs');
const path = require('node:path');
const args = process.argv.slice(2);
fs.appendFileSync(process.env.FIXTURE_EVENTS, JSON.stringify({kind:'cli',args})+'\n');
if (args.length !== 2 || !['chromium-headless-shell','chromium','firefox'].includes(args[1])) {
  console.error('fixture rejected installer arguments');
  process.exit(98);
}
if (args[0] === 'install') {
  const status = Number(process.env.FIXTURE_DOWNLOAD_STATUS);
  if (status) process.exit(status);
  fs.writeFileSync(path.join(process.env.FIXTURE_ROOT,'downloaded'), 'ready');
} else if (args[0] === 'install-deps') {
  const status = Number(process.env.FIXTURE_SETUP_STATUS);
  if (status) process.exit(status);
  fs.writeFileSync(path.join(process.env.FIXTURE_ROOT,'dependencies'), 'ready');
} else {
  process.exit(98);
}
"#,
        )
        .unwrap();
        fs::write(
            package.join("index.cjs"),
            r#"const fs = require('node:fs');
const path = require('node:path');
const log = event => fs.appendFileSync(process.env.FIXTURE_EVENTS,JSON.stringify(event)+'\n');
function browser(name) {
  return {
    executablePath() { return '/fixture/full-'+name; },
    async launch(options) {
      log({kind:'launch',browser:name,options});
      if (!fs.existsSync(path.join(process.env.FIXTURE_ROOT,'downloaded'))) {
        throw new Error('fixture browser was not downloaded');
      }
      if (process.env.FIXTURE_ALWAYS_FAIL === '1' ||
          (process.env.FIXTURE_MISSING_DEPS === '1' &&
           !fs.existsSync(path.join(process.env.FIXTURE_ROOT,'dependencies')))) {
        throw new Error('fixture browser launch unavailable');
      }
      return {async close() {
        log({kind:'close',browser:name});
        if (process.env.FIXTURE_CLOSE_FAIL === '1') throw new Error('fixture browser close failed');
      }};
    },
  };
}
module.exports = {chromium:browser('chromium'),firefox:browser('firefox')};
"#,
        )
        .unwrap();
        let events = root.path().join("events.jsonl");
        Self {
            root,
            script,
            events,
        }
    }

    /// Run from an unrelated directory so lookup cannot depend on the working directory.
    fn run(&self, arguments: &[&str], overrides: &[(&str, &str)]) -> Output {
        Command::new("node")
            .arg(&self.script)
            .args(arguments)
            .current_dir(self.root.path().join("elsewhere"))
            .env("FIXTURE_EVENTS", &self.events)
            .env("FIXTURE_ROOT", self.root.path())
            .env("FIXTURE_DOWNLOAD_STATUS", "0")
            .env("FIXTURE_SETUP_STATUS", "97")
            .env("FIXTURE_MISSING_DEPS", "0")
            .env("FIXTURE_ALWAYS_FAIL", "0")
            .env("FIXTURE_CLOSE_FAIL", "0")
            .envs(overrides.iter().copied())
            .output()
            .expect("run browser installer with Node")
    }

    /// Decode the exact command, launch and close history, including failed stages.
    fn events(&self) -> Vec<Value> {
        let contents = match fs::read_to_string(&self.events) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return vec![],
            Err(error) => panic!("read browser fixture events: {error}"),
        };
        contents
            .lines()
            .map(|line| serde_json::from_str(line).expect("decode fixture event"))
            .collect()
    }
}

/// Identify the launched public browser and require the intended executable and timeout.
fn assert_launch(event: &Value, target: &str) {
    assert_eq!(event["kind"], "launch", "{event}");
    assert_eq!(
        event["browser"],
        if target == "firefox" {
            "firefox"
        } else {
            "chromium"
        },
        "{event}"
    );
    let options = event["options"].as_object().expect("launch options");
    assert_eq!(options.get("timeout"), Some(&json!(30_000)), "{event}");
    assert!(
        options
            .get("headless")
            .is_none_or(|value| value == &json!(true)),
        "preflight must use headless mode: {event}"
    );
    if target == "chromium" {
        assert_eq!(
            options.get("executablePath"),
            Some(&json!("/fixture/full-chromium")),
            "full Chromium must launch the full browser binary: {event}"
        );
    } else {
        assert!(
            !options.contains_key("executablePath"),
            "headless shell and Firefox must retain their default executable: {event}"
        );
    }
}

/// Check that a run has no extra commands or browser operations after a terminal failure.
fn assert_history(events: &[Value], expected: &[&str]) {
    let actual: Vec<_> = events
        .iter()
        .map(|event| {
            if event["kind"] == "cli" {
                event["args"][0].as_str().expect("CLI operation")
            } else {
                event["kind"].as_str().expect("fixture event kind")
            }
        })
        .collect();
    assert_eq!(actual, expected, "complete fixture history: {events:?}");
}

#[test]
/// A runnable browser must skip host package installation even when it is permitted.
fn runnable_browsers_download_and_launch_without_host_package_setup() {
    if !node_available() {
        return;
    }
    for target in ["chromium-headless-shell", "chromium", "firefox"] {
        for permitted in [false, true] {
            let fixture = BrowserFixture::new();
            let mut arguments = vec![target];
            if permitted {
                arguments.push("--with-deps");
            }
            let output = fixture.run(&arguments, &[]);
            assert!(
                output.status.success(),
                "target={target}, permitted={permitted}: {output:?}"
            );
            let events = fixture.events();
            assert_history(&events, &["install", "launch", "close"]);
            assert_eq!(events[0]["args"], json!(["install", target]));
            assert_launch(&events[1], target);
        }
    }
}

#[test]
/// A failed launch may install dependencies once, then must launch and close successfully.
fn missing_host_dependencies_require_a_successful_public_browser_retry() {
    if !node_available() {
        return;
    }
    for target in ["chromium-headless-shell", "chromium", "firefox"] {
        let fixture = BrowserFixture::new();
        let output = fixture.run(
            &[target, "--with-deps"],
            &[("FIXTURE_MISSING_DEPS", "1"), ("FIXTURE_SETUP_STATUS", "0")],
        );
        assert!(output.status.success(), "target={target}: {output:?}");
        let events = fixture.events();
        assert_history(
            &events,
            &["install", "launch", "install-deps", "launch", "close"],
        );
        assert_eq!(events[0]["args"], json!(["install", target]));
        assert_eq!(events[2]["args"], json!(["install-deps", target]));
        assert_launch(&events[1], target);
        assert_launch(&events[3], target);
    }
}

#[test]
/// Download and host setup failures keep their original exit status and stop immediately.
fn browser_installer_preserves_download_and_dependency_setup_exit_codes() {
    if !node_available() {
        return;
    }
    for (name, overrides, code, history) in [
        (
            "download",
            vec![("FIXTURE_DOWNLOAD_STATUS", "23")],
            23,
            vec!["install"],
        ),
        (
            "host setup",
            vec![
                ("FIXTURE_MISSING_DEPS", "1"),
                ("FIXTURE_SETUP_STATUS", "43"),
            ],
            43,
            vec!["install", "launch", "install-deps"],
        ),
    ] {
        let fixture = BrowserFixture::new();
        let output = fixture.run(&["chromium-headless-shell", "--with-deps"], &overrides);
        assert_eq!(output.status.code(), Some(code), "{name}: {output:?}");
        assert_history(&fixture.events(), &history);
    }
}

#[test]
/// Successful package setup cannot turn a second browser launch failure into a pass.
fn browser_installer_rejects_a_persistent_launch_failure_after_one_setup_attempt() {
    if !node_available() {
        return;
    }
    let fixture = BrowserFixture::new();
    let output = fixture.run(
        &["chromium", "--with-deps"],
        &[("FIXTURE_ALWAYS_FAIL", "1"), ("FIXTURE_SETUP_STATUS", "0")],
    );
    assert!(!output.status.success(), "{output:?}");
    let events = fixture.events();
    assert_history(&events, &["install", "launch", "install-deps", "launch"]);
    assert_launch(&events[1], "chromium");
    assert_launch(&events[3], "chromium");
}

#[test]
/// Close failures remain fatal and never trigger dependency installation or another launch.
fn browser_installer_does_not_treat_close_failures_as_missing_dependencies() {
    if !node_available() {
        return;
    }
    for recovered in [false, true] {
        let fixture = BrowserFixture::new();
        let mut overrides = vec![("FIXTURE_CLOSE_FAIL", "1")];
        if recovered {
            overrides.extend([("FIXTURE_MISSING_DEPS", "1"), ("FIXTURE_SETUP_STATUS", "0")]);
        }
        let output = fixture.run(&["firefox", "--with-deps"], &overrides);
        assert!(
            !output.status.success(),
            "recovered={recovered}: {output:?}"
        );
        assert_history(
            &fixture.events(),
            if recovered {
                &["install", "launch", "install-deps", "launch", "close"]
            } else {
                &["install", "launch", "close"]
            },
        );
    }
}

#[test]
/// Callers that omit permission for host setup must stop after the initial failed launch.
fn browser_installer_without_dependency_setup_permission_stops_on_launch_failure() {
    if !node_available() {
        return;
    }
    let fixture = BrowserFixture::new();
    let output = fixture.run(
        &["chromium-headless-shell"],
        &[("FIXTURE_MISSING_DEPS", "1")],
    );
    assert!(!output.status.success(), "{output:?}");
    assert_history(&fixture.events(), &["install", "launch"]);
}

#[test]
/// Invalid targets, duplicate flags and extra arguments must fail before any side effects.
fn browser_installer_rejects_invalid_arguments_before_commands_or_launches() {
    if !node_available() {
        return;
    }
    for arguments in [
        vec![],
        vec!["webkit"],
        vec!["chromium", "--unknown"],
        vec!["chromium", "--with-deps", "--with-deps"],
        vec!["chromium", "firefox"],
        vec!["--with-deps", "chromium"],
    ] {
        let fixture = BrowserFixture::new();
        let output = fixture.run(&arguments, &[]);
        assert!(
            !output.status.success(),
            "arguments={arguments:?}: {output:?}"
        );
        assert!(
            fixture.events().is_empty(),
            "invalid arguments must not start commands or browsers: {arguments:?}"
        );
    }
}
