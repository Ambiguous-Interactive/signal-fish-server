# ADR: CI/CD Preventative Measures

**Status:** Accepted
**Date:** 2026-02-16
**Author:** Claude Sonnet 4.5 (Anthropic)
**Deciders:** Ambiguous Interactive Engineering Team

## Context

After fixing multiple CI/CD issues in the Signal Fish Server repository, we identified the need for systematic preventative measures to catch similar problems early:

### Issues Fixed

1. **YAML Lint Python Cache Issue** (yaml-lint.yml)
   - **Problem:** Python pip caching was configured in a Rust-only project
   - **Impact:** Unnecessary cache setup, potential confusion for future maintainers
   - **Root Cause:** Language-specific cache configuration without project type validation

2. **Nightly Toolchain Staleness** (unused-deps.yml)
   - **Problem:** Nightly toolchain was 360+ days old
   - **Impact:** Potential compatibility issues, missing important bug fixes
   - **Root Cause:** No automated detection of stale nightly versions

3. **Unused Dependencies** (Cargo.toml)
   - **Problem:** Dependencies declared but not actually used in code
   - **Impact:** Larger binary size, increased attack surface, maintenance burden
   - **Root Cause:** No automated validation that dependencies are actually used

4. **Markdown Linting Issues** (MD040, MD060 rules)
   - **Problem:** Missing language identifiers on code blocks, table alignment issues
   - **Impact:** CI failures, inconsistent documentation formatting
   - **Root Cause:** No local validation of markdown files before commit

5. **Typos Configuration Missing** (.typos.toml)
   - **Problem:** Technical terms like "HashiCorp" flagged as typos
   - **Impact:** CI failures, false positive spell check errors
   - **Root Cause:** No configuration for technical terminology

### Risk Assessment

Without systematic preventative measures, these types of issues will recur as the project evolves:
- CI failures become harder to debug
- Configuration drift causes mysterious failures
- Developer productivity decreases
- Security vulnerabilities may be introduced through stale dependencies

## Decision

Implement comprehensive CI/CD preventative measures across six layers:

### 1. Workflow Hygiene Validation

**New Files:**
- `/scripts/check-workflow-hygiene.sh` - Standalone validation script
- `/.github/workflows/workflow-hygiene.yml` - CI integration

**Checks Implemented:**
- **Language-specific caching validation**: Detects Python/Node cache on Rust projects
- **Nightly toolchain staleness**: Warns at 180 days, errors at 365 days
- **Workflow self-validation**: Ensures actionlint, yamllint, shellcheck are present
- **Dependency audit workflows**: Validates cargo-deny, cargo-machete, cargo-udeps exist
- **Timeout configurations**: Checks that jobs have timeout-minutes to prevent hung builds
- **Action pinning**: Validates GitHub Actions are pinned to SHA hashes

**Schedule:**
- Runs on workflow file changes (push/PR to main)
- Weekly cron job (Monday 06:00 UTC) for proactive staleness detection

### 2. Data-Driven CI Configuration Tests

**New File:** `/tests/ci_config_tests.rs`

**Test Coverage:**
- **MSRV consistency**: Validates rust-version matches across Cargo.toml, rust-toolchain.toml, clippy.toml, Dockerfile
- **Required workflows exist**: Ensures critical workflows (ci.yml, yaml-lint.yml, etc.) are present
- **CI workflow jobs**: Validates main CI has required jobs (check, test, deny, msrv, docker)
- **YAML validity**: Basic YAML syntax validation (balanced quotes, required fields)
- **Cache configuration**: Prevents language-specific cache mismatches
- **Script permissions**: Ensures shell scripts have executable permissions
- **Markdown validation**: Checks for code blocks without language identifiers (MD040)
- **Typos configuration**: Validates .typos.toml exists and has required structure
- **Markdownlint configuration**: Validates .markdownlint.json exists and is properly configured

**Integration:**
- Runs as part of `cargo test` (included in standard test suite)
- Fast execution (< 1 second)
- Clear, actionable error messages

### 3. Enhanced MSRV Enforcement

**Already Implemented (commit d9eac0f):**
- `/scripts/check-msrv-consistency.sh` - Standalone MSRV validation
- CI job in `.github/workflows/ci.yml` - MSRV verification job
- `/.llm/skills/msrv-and-toolchain-management.md` - Comprehensive guide

**Improvements Made:**
- Single source of truth (Cargo.toml `rust-version`)
- Automated consistency validation across all config files
- Build and test with exact MSRV in CI
- Clear error messages with remediation steps

### 4. Markdown Linting and Validation

**New Files:**
- `/scripts/check-markdown.sh` - Local markdown validation script
- Tests added to `/tests/ci_config_tests.rs` - Markdown validation tests

**Checks Implemented:**
- **MD040 validation**: Ensures all code blocks have language identifiers
- **Configuration validation**: Validates .markdownlint.json exists and is properly configured
- **Auto-fix capability**: Script can automatically fix common markdown issues
- **Test coverage**: Validates markdown files during `cargo test`

**Integration:**
- Pre-commit hook runs markdown linting (if markdownlint-cli2 is installed)
- CI validates markdown files on every PR
- VS Code extension recommendations for real-time linting

### 5. Spell Checking Configuration

**Enhanced Files:**
- `.typos.toml` - Comprehensive technical term configuration
- Tests added to `/tests/ci_config_tests.rs` - Typos config validation

**Configuration Coverage:**
- Rust crate names and tooling terms
- Build tools and infrastructure (HashiCorp, GitHub, Docker, etc.)
- Game engines and networking protocols
- WebSocket and API terminology
- Project-specific terms

**Validation:**
- Test ensures .typos.toml exists and has required sections
- Warns if common technical terms are missing
- CI runs typos check on every commit

### 6. Documentation and Guidance

**New Documentation:**
- `/docs/adr/ci-cd-preventative-measures.md` (this document)
- Enhanced workflow comments explaining cache decisions
- Nightly toolchain documentation in unused-deps.yml
- Markdown linting section in `/docs/development.md`

**New VS Code Configuration:**
- `/.vscode/extensions.json` - Recommended extensions for markdown linting and spell checking
- `/.vscode/settings.json` - Enhanced with markdown formatting and spell check configuration

**Existing Documentation Enhanced:**
- `.llm/skills/msrv-and-toolchain-management.md` - Toolchain management
- `.llm/skills/github-actions-best-practices.md` - Workflow best practices
- `.llm/skills/dependency-management.md` - Dependency hygiene
- `/docs/development.md` - Added markdown linting, spell checking, and CI sections

## Consequences

### Benefits

1. **Early Detection:**
   - Catch CI issues during development (local testing)
   - Prevent broken configs from reaching main branch
   - Reduce CI failure investigation time

2. **Developer Productivity:**
   - Clear, actionable error messages
   - Automated validation (no manual checking)
   - Documentation explains "why" not just "what"

3. **Maintainability:**
   - Data-driven tests scale as project grows
   - Weekly staleness detection catches bitrot
   - Self-documenting through tests and scripts

4. **Security:**
   - Detect stale toolchains automatically
   - Validate dependency usage
   - Ensure security audit workflows are present

### Trade-offs

1. **Additional CI Time:**
   - Workflow hygiene check adds ~10 seconds
   - CI config tests add ~1 second to test suite
   - **Mitigation:** Fast execution, only runs on workflow changes or weekly

2. **Maintenance Burden:**
   - Scripts need updates as project evolves
   - Tests may need adjustment for new workflows
   - **Mitigation:** Well-documented, simple bash/Rust, clear ownership

3. **False Positives:**
   - Nightly documentation detection may have edge cases
   - Cache validation may flag legitimate exceptions
   - **Mitigation:** Scripts output warnings (not errors) for subjective checks

### Risks

1. **Script Maintenance:**
   - Risk: Scripts become stale or incorrect
   - Mitigation: Scripts themselves are tested by CI config tests
   - Mitigation: Clear documentation of what each check validates

2. **Test Brittleness:**
   - Risk: Tests break with legitimate config changes
   - Mitigation: Tests validate intent (required workflows) not implementation
   - Mitigation: Clear error messages explain how to fix

## Implementation Details

### Workflow Hygiene Script

**Key Features:**
- Color-coded output (info/warn/error)
- Exit code 0 for warnings, 1 for errors
- Can run locally or in CI
- Shellcheck validated (no warnings)

**Example Output:**
```text
[OK] No language-specific caching mismatches found
[OK] unused-deps.yml: Nightly toolchain is recent (< 6 months old)
[WARN] ci.yml: No timeout-minutes found (consider adding)
[OK] All 44 actions are pinned to SHA hashes
```

### CI Config Tests

**Test Philosophy:**
- **Intent-based**: Test what matters (MSRV consistency), not formatting
- **Actionable failures**: Error messages include fix instructions
- **Fast execution**: No external tools, pure Rust file reading
- **Data-driven**: Easy to add new validation rules

**Example Test:**
```rust
#[test]
fn test_msrv_consistency_across_config_files() {
    // Single source of truth: Cargo.toml rust-version
    let msrv = extract_toml_version(&cargo_content, "rust-version");

    // Validate rust-toolchain.toml
    assert_eq!(toolchain_version, msrv, "Fix: Update rust-toolchain.toml");
}
```

### Integration Strategy

**Local Development:**
1. Run `./scripts/check-workflow-hygiene.sh` before pushing workflow changes
2. Run `cargo test` (includes CI config tests automatically)
3. Optional: Add to git pre-commit hook

**CI Pipeline:**
1. Workflow hygiene runs on workflow file changes (fast path)
2. CI config tests run as part of standard test suite
3. Weekly cron for proactive staleness detection
4. All checks provide clear remediation steps

## Alternatives Considered

### Alternative 1: Manual Code Review Only
**Rejected:** Human review misses subtle issues, not scalable

### Alternative 2: External CI Validation Tools
**Rejected:** Adds dependencies, not tailored to our specific issues

### Alternative 3: Pre-commit Hooks Only
**Rejected:** Developers can bypass, doesn't catch drift over time

### Alternative 4: More Complex YAML Parsing
**Rejected:** Adds dependencies (serde_yaml), increases complexity unnecessarily

## Validation

### Success Criteria

1. ✅ **Workflow hygiene script runs successfully**
   - Validates all workflow files without errors
   - Detects nightly toolchain age correctly
   - Identifies cache mismatches

2. ✅ **CI config tests pass**
   - All 9 tests pass in clean repository (MSRV, workflows, YAML, cache, scripts, markdown, typos)
   - Tests detect MSRV inconsistency (tested by temporarily breaking config)
   - Tests detect missing workflows (tested by moving file)
   - Tests detect markdown issues (MD040 violations caught)

3. ✅ **Integration works end-to-end**
   - New workflow-hygiene.yml workflow syntax is valid
   - Scripts are executable and shellcheck clean
   - Tests run as part of `cargo test`
   - Pre-commit hook includes markdown linting

4. ✅ **Documentation is clear**
   - ADR explains rationale and implementation
   - Scripts have usage documentation
   - Error messages include remediation steps
   - Development.md updated with markdown linting and spell checking sections

5. ✅ **Developer tooling configured**
   - VS Code extensions recommended (markdownlint, spell checker)
   - VS Code settings configured for markdown formatting
   - Pre-commit hook updated with markdown linting

### Testing Performed

```bash
# Validate workflow hygiene script
./scripts/check-workflow-hygiene.sh
# Result: PASS (4 warnings, 0 errors)

# Validate CI config tests (including new markdown and typos tests)
cargo test --test ci_config_tests
# Result: 9 passed; 0 failed

# Validate markdown linting script
./scripts/check-markdown.sh
# Result: All markdown files are valid

# Validate shellcheck
shellcheck -s bash scripts/check-workflow-hygiene.sh scripts/check-markdown.sh
# Result: No warnings

# Validate workflow syntax
yamllint .github/workflows/workflow-hygiene.yml
# Result: PASS

# Validate scripts are executable
ls -la scripts/*.sh
# Result: All have +x permission
```

## Future Enhancements

### Short Term (1-3 months)
1. Add integration test that temporarily breaks config and validates detection
2. Expand CI config tests to cover more edge cases
3. Add metric tracking for CI failure reasons

### Long Term (3-12 months)
1. Automated nightly toolchain update PRs (GitHub Actions bot)
2. Dependency staleness detection (unused deps not updated in X months)
3. CI performance regression detection
4. Supply chain security scanning integration

## References

- **Related Commits:**
  - d9eac0f: Add MSRV consistency enforcement
  - a7974e2: CI / CD fixes (added test fixtures and validation)
  - 4b9b60d: CI updates (enhanced doc-validation workflow)

- **Related Documentation:**
  - `.llm/skills/msrv-and-toolchain-management.md`
  - `.llm/skills/github-actions-best-practices.md`
  - `.llm/skills/dependency-management.md`
  - `.github/test-fixtures/README.md`

- **Related Workflows:**
  - `.github/workflows/ci.yml` (MSRV verification job)
  - `.github/workflows/unused-deps.yml` (dependency hygiene)
  - `.github/workflows/yaml-lint.yml` (workflow syntax validation)
  - `.github/workflows/workflow-hygiene.yml` (new preventative measure)

## Review and Maintenance

**Review Schedule:** Quarterly
**Owner:** CI/CD Infrastructure Team
**Next Review:** 2026-05-16

**Review Checklist:**
- [ ] Are all checks still relevant?
- [ ] Have new issues emerged that need detection?
- [ ] Are error messages still clear and actionable?
- [ ] Is performance acceptable (< 30s total CI overhead)?
- [ ] Are there new GitHub Actions best practices to incorporate?

---

**Changelog:**
- 2026-02-16: Initial ADR created with comprehensive preventative measures
- 2026-02-16: Updated with markdown linting and spell checking preventative measures (layers 4-5)
- 2026-02-16: Fixed HashiCorp typo false positive by adding to [default.extend-identifiers] section
