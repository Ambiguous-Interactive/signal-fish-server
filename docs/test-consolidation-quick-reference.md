# Test Consolidation Quick Reference

## At a Glance

```text
Current: 35 tests, 2,492 lines, ~40% duplication
Target:  19 tests, 1,800 lines, ~10% duplication
Savings: -45% tests, -27% code, +62% data-driven
```

## Top Consolidation Targets

| Category | Before | After | Savings | Priority |
|----------|--------|-------|---------|----------|
| MSRV | 4 | 2 | 50% | 🔴 HIGH |
| Workflow Validation | 7 | 3 | 57% | 🔴 HIGH |
| Best Practices | 3 | 1 | 67% | 🟡 MEDIUM |
| Actions Security | 3 | 1 | 67% | 🟡 MEDIUM |
| Markdown | 7 | 3 | 57% | 🟡 MEDIUM |
| Configuration | 6 | 3 | 50% | 🟢 LOW |
| AWK Scripts | 4 | 2 | 50% | 🟢 LOW |

## Data-Driven Pattern Template

```rust
// 1. Define specification structure
struct ValidationSpec {
    name: &'static str,
    path: &'static str,
    required_fields: Vec<&'static str>,
    validator: fn(&str) -> Vec<String>,
}

// 2. Define specs as data
const SPECS: &[ValidationSpec] = &[
    ValidationSpec {
        name: "example",
        path: "example.toml",
        required_fields: vec!["field1", "field2"],
        validator: validate_example,
    },
    // ... more specs
];

// 3. Single test iterates specs
#[test]
fn test_validations() {
    let mut violations = Vec::new();

    for spec in SPECS {
        let content = read_file(spec.path);

        for field in &spec.required_fields {
            if !content.contains(field) {
                violations.push(format!(
                    "{}: Missing {}\nFix: Add {} = \"value\"",
                    spec.name, field, field
                ));
            }
        }

        violations.extend((spec.validator)(&content));
    }

    assert!(violations.is_empty(), "{}", violations.join("\n\n"));
}
```

## Error Message Template

```rust
panic!(
    "{}: Validation failed\n\
     \n\
     Current: {}\n\
     Expected: {}\n\
     \n\
     Why this matters:\n\
     - Reason 1\n\
     - Reason 2\n\
     \n\
     Fix with:\n\
     {}\n\
     \n\
     Or manually:\n\
     {}",
    file, current, expected, fix_command, manual_steps
);
```

## Helper Functions

```rust
// File operations
fn read_file_or_panic(path: &Path, context: &str) -> String;
fn find_files(root: &Path, ext: &[&str], exclude: &[&str]) -> Vec<PathBuf>;

// Format validation
fn validate_yaml_syntax(content: &str) -> Vec<String>;
fn validate_toml_syntax(content: &str) -> Vec<String>;
fn validate_json_syntax(content: &str) -> Vec<String>;

// Version handling
fn to_major_minor(version: &str) -> String;
fn version_matches(v1: &str, v2: &str, allow_shorthand: bool) -> bool;

// Reporting
fn format_violations(violations: &[Violation]) -> String;
fn report_stats(stats: &ValidationStats) -> String;
```

## Common Validation Patterns

### Pattern 1: File Existence
```rust
const REQUIRED_FILES: &[(&str, &str)] = &[
    ("Cargo.toml", "Rust manifest"),
    (".gitignore", "Git ignore rules"),
];

for (file, desc) in REQUIRED_FILES {
    let path = root.join(file);
    assert!(path.exists(), "Missing {}: {}", desc, path.display());
}
```

### Pattern 2: Config Field Validation
```rust
const REQUIRED_CONFIG_FIELDS: &[(&str, &str, &str)] = &[
    ("clippy.toml", "msrv", "1.88.0"),
    ("rust-toolchain.toml", "channel", "1.88.0"),
];

for (file, field, expected) in REQUIRED_CONFIG_FIELDS {
    let content = read_file(root.join(file));
    let value = extract_field(&content, field);
    assert_eq!(value, *expected, "{}: {} mismatch", file, field);
}
```

### Pattern 3: Workflow Action Validation
```rust
const REQUIRED_ACTIONS: &[(&str, &[&str])] = &[
    ("ci.yml", &["actions/checkout", "actions/cache"]),
    ("link-check.yml", &["lycheeverse/lychee-action"]),
];

for (workflow, actions) in REQUIRED_ACTIONS {
    let content = read_file(workflows_dir.join(workflow));
    for action in *actions {
        assert!(content.contains(action), "{}: Missing {}", workflow, action);
    }
}
```

## Migration Checklist

### For Each Consolidation:

- [ ] Create data structure for specs
- [ ] Implement consolidated test
- [ ] Run both old and new tests
- [ ] Verify same coverage
- [ ] Improve error messages
- [ ] Add statistics/reporting
- [ ] Mark old tests `#[ignore]`
- [ ] Wait 1 week (CI validation)
- [ ] Remove old tests
- [ ] Update documentation

## Testing Commands

```bash
# Run specific test
cargo test test_name -- --nocapture

# Run all CI config tests
cargo test --test ci_config_tests

# Run with verbose output
cargo test --test ci_config_tests -- --nocapture --test-threads=1

# Check for warnings
cargo test --test ci_config_tests 2>&1 | grep -i "warning"
```

## Common Pitfalls

### ❌ Don't:
- Remove old tests immediately
- Change validation logic while consolidating
- Skip error message improvements
- Forget to add statistics

### ✅ Do:
- Keep old tests with `#[ignore]` initially
- Preserve exact validation logic
- Improve error messages during consolidation
- Add diagnostic information

## Example: MSRV Consolidation

### Before (4 tests, 225 lines)
```rust
#[test] fn test_msrv_consistency_across_config_files() { /* ... */ }
#[test] fn test_msrv_version_normalization_logic() { /* ... */ }
#[test] fn test_ci_workflow_msrv_normalization() { /* ... */ }
#[test] fn test_msrv_script_consistency_with_ci() { /* ... */ }
```

### After (2 tests, 150 lines)
```rust
const MSRV_CONFIG_FILES: &[MsrvConfigFile] = &[/* ... */];
const MSRV_SCRIPTS: &[&str] = &[/* ... */];

#[test] fn test_msrv_version_normalization_logic() { /* pure unit test */ }
#[test] fn test_msrv_consistency() { /* data-driven validation */ }
```

**Benefits:**
- -50% test count
- -33% lines of code
- +100% data-driven
- Easier to add new files

## Statistics Structure

```rust
struct ValidationStats {
    total_checked: usize,
    violations_found: usize,
    warnings: usize,
    files_processed: usize,
}

impl fmt::Display for ValidationStats {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Checked: {}, Violations: {}, Warnings: {}, Files: {}",
            self.total_checked,
            self.violations_found,
            self.warnings,
            self.files_processed
        )
    }
}
```

## Severity Levels

```rust
enum Severity {
    Error,   // Test fails
    Warning, // Printed to stderr, test passes
    Info,    // Informational only
}

// Separate violations by severity
let (errors, warnings, info): (Vec<_>, Vec<_>, Vec<_>) =
    violations.iter().partition_by_severity();

// Report non-errors
if !warnings.is_empty() {
    eprintln!("Warnings:\n{}", format_violations(&warnings));
}

// Fail only on errors
assert!(errors.is_empty(), "Errors:\n{}", format_violations(&errors));
```

## Resources

- **Full Analysis:** `docs/test-suite-analysis-ci-config.md`
- **Code Examples:** `docs/test-consolidation-examples.md`
- **Summary:** `docs/test-suite-recommendations-summary.md`

## Quick Start

1. Read the summary document (5 minutes)
2. Review MSRV consolidation example (10 minutes)
3. Implement MSRV consolidation (1 hour)
4. Test and validate (30 minutes)
5. Apply pattern to other tests

**Total time to first consolidation: ~2 hours**

---

**Last Updated:** 2026-02-17
**Status:** Ready for implementation
