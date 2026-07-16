# Testing CI and Coverage Patterns

**Applies to**: When writing tests for CI configuration, smoke tests, or markdown/link validation.

---

## When to Use

- Writing smoke tests for Docker deployment artifacts
- Validating configuration defaults work in production scenarios
- Testing markdown quality and link integrity as part of CI
- Adding CI-specific integration tests

---

## When NOT to Use

- Core application test patterns (see [Testing Core Patterns](./testing-core-patterns.md))
- Test error message design (see [Testing Error Message Quality](./testing-error-message-quality.md))
- CI/CD workflow configuration (see [GitHub Actions Workflow Config](../../ci-troubleshooting/references/github-actions-workflow-config.md))

---

## TL;DR

- Smoke tests must use a retry loop — never a bare `sleep`.
- Dump container logs on failure for diagnostics.
- Test configuration defaults in Docker mode (no config file, ENV overrides).
- Markdown tests run as `cargo test` — no external tools needed.

---

## Config Validation Tests

Always test that configuration defaults work in production deployment scenarios:

```rust
#[test]
fn test_docker_default_config_passes_validation() {
    // Simulate Docker ENV overrides (auth disabled, no config file)
    let mut config = Config::default();
    config.security.require_metrics_auth = false;
    config.security.require_websocket_auth = false;
    assert!(validate_config_security(&config).is_ok());
}

#[test]
fn test_config_with_all_features_loads() {
    // Ensure config loads with all cargo features enabled
    let config = Config::from_env().unwrap();
    assert!(config.validate().is_ok());
}
```

---

## Smoke Test Patterns

CI smoke tests must verify the complete deployment artifact:

```yaml
# GitHub Actions example
- name: Smoke test
  run: |
    docker run -d --name test-server -p 3536:3536 signal-fish-server:ci
    # Retry loop instead of bare sleep
    for i in $(seq 1 15); do
      if curl -sf http://localhost:3536/v2/health; then
        echo "Health check passed on attempt $i/15"
        exit 0
      fi
      echo "Attempt $i/15: server not ready, retrying in 2s..."
      sleep 2
    done
    echo "ERROR: Server failed to become healthy after 30s"
    echo "=== Docker logs ==="
    docker logs test-server
    exit 1
```

**Key smoke test requirements:**

- Retry loop with timeout (not bare `sleep`)
- Dump logs on failure for diagnostics
- Test default configuration (no mounted config files)
- Verify all critical endpoints (health, metrics, WebSocket upgrade)

---

## File Path Case Sensitivity Tests

```rust
#[test]
fn test_skill_links_case_sensitive() {
    // Verify all skill file links use correct case (prevents Linux CI failures)
    let context_file = std::fs::read_to_string("AGENTS.md").unwrap();
    for (skill_name, skill_path) in extract_skill_links(&context_file) {
        assert!(
            std::path::Path::new(skill_path).exists(),
            "Skill link broken: {skill_name} -> {skill_path}"
        );
    }
}
```

---

## CI-Specific Integration Tests

```rust
#[cfg(test)]
mod ci_integration_tests {
    use super::*;

    #[test]
    #[ignore = "runs only in CI"]
    fn test_all_features_compile() {
        // This test verifies --all-features builds succeed
        // Ignored by default, runs only in CI via `cargo test -- --ignored`
    }

    #[test]
    fn test_native_deps_available() {
        // Verify native dependencies required by optional features are present
        #[cfg(feature = "kafka")]
        {
            // Test that rdkafka native lib is available
            let _ = rdkafka::ClientConfig::new();
        }
    }
}
```

---

## Markdown Validation Tests

Validate markdown quality and link integrity as part of the test suite:

```rust
#[test]
fn test_markdown_files_have_language_identifiers() {
    let markdown_files = find_markdown_files(&repo_root());
    let mut violations = Vec::new();

    for file in markdown_files {
        let content = read_file(&file);

        for (line_num, line) in content.lines().enumerate() {
            // Check for opening code fence without language
            let fence_marker = "```";
            if line.trim_start().starts_with(fence_marker) {
                let fence_content = line.trim_start()
                    .trim_start_matches('`')
                    .trim();

                if fence_content.is_empty() {
                    violations.push(format!(
                        "{}:{}: Code block missing language identifier (MD040)",
                        file.display(),
                        line_num + 1
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Found code blocks without language identifiers:\n{}\n\n\
         Fix: Add language after opening backticks (e.g., ```rust, ```bash, ```text)",
        violations.join("\n")
    );
}

#[test]
fn test_markdown_links_case_sensitive() {
    // Verify all internal markdown links use correct filename case
    let markdown_files = find_markdown_files(&repo_root());
    let mut broken_links = Vec::new();

    for md_file in markdown_files {
        let content = read_file(&md_file);
        let links = extract_internal_links(&content);

        for (line_num, link) in links {
            let target = resolve_link_target(&md_file, &link);

            if let Some(target_path) = target {
                if !target_path.exists() {
                    broken_links.push(format!(
                        "{}:{}: Broken link (case sensitivity?): {}",
                        md_file.display(),
                        line_num,
                        link
                    ));
                }
            }
        }
    }

    assert!(
        broken_links.is_empty(),
        "Found broken internal links:\n{}\n\n\
         Note: Links are case-sensitive on Linux. Verify exact filename case.",
        broken_links.join("\n")
    );
}

#[test]
fn test_lychee_config_exists() {
    let lychee_config = repo_root().join(".lychee.toml");

    assert!(
        lychee_config.exists(),
        ".lychee.toml is required for link checking in CI"
    );

    let content = read_file(&lychee_config);

    // Verify critical exclusions are present
    assert!(
        content.contains("exclude = ["),
        ".lychee.toml must have exclusion patterns for placeholder URLs"
    );
}

#[test]
fn test_markdownlint_config_exists() {
    let config = repo_root().join(".markdownlint.json");

    assert!(
        config.exists(),
        ".markdownlint.json is required for markdown linting.\n\
         Create with: echo '{{\"MD040\": true, \"MD013\": false}}' > .markdownlint.json"
    );
}

#[test]
fn test_typos_config_has_required_sections() {
    let typos_config = repo_root().join(".typos.toml");

    assert!(
        typos_config.exists(),
        ".typos.toml is required for spell checking"
    );

    let content = read_file(&typos_config);

    assert!(
        content.contains("[default.extend-words]"),
        ".typos.toml must have [default.extend-words] section for lowercase technical terms"
    );

    assert!(
        content.contains("[default.extend-identifiers]"),
        ".typos.toml must have [default.extend-identifiers] section for mixed-case company names"
    );
}
```

**Key patterns for markdown validation:**

1. **Data-driven approach**: Test all markdown files, don't hardcode filenames
2. **Clear error messages**: Include file path, line number, and fix instructions
3. **Fast execution**: Pure file reading, no external tools
4. **CI integration**: Run as part of `cargo test`, no special setup needed

---

## Agent Checklist

- [ ] Smoke tests use retry loop with timeout (not bare `sleep`)
- [ ] Docker logs dumped on smoke test failure
- [ ] Default config tested in Docker mode (ENV overrides, no config file)
- [ ] CI-only tests marked with `#[ignore = "runs only in CI"]`
- [ ] Markdown tests are data-driven (scan all files, not hardcoded)
- [ ] Markdown test error messages include file path and line number

---

## Related References

- [Testing Core Patterns](./testing-core-patterns.md) — Core testing methodology and patterns
- [Testing Error Message Quality](./testing-error-message-quality.md) — Actionable test failure messages
- [Test Fixture Structure](./test-fixture-structure.md) — Data-driven CI configuration test fixtures
- [Clippy And Linting](../../rust-development/references/clippy-and-linting.md) — CI pipeline integration
- [GitHub Actions Workflow Config](../../ci-troubleshooting/references/github-actions-workflow-config.md) — GitHub Actions workflow patterns and debugging
