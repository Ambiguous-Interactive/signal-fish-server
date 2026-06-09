#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;

fn run_validator_with_workflow(workflow_content: &str) -> (bool, String) {
    let temp_root = unique_temp_dir("workflow-awk-validator");
    let script_src = repo_root().join("scripts/validate-workflow-awk.sh");
    let script_dst = temp_root.path().join("scripts/validate-workflow-awk.sh");
    let scanner_src = repo_root().join("scripts/validate-workflow-awk-scanner.awk");
    let scanner_dst = temp_root
        .path()
        .join("scripts/validate-workflow-awk-scanner.awk");
    let workflow_path = temp_root.path().join(".github/workflows/test.yml");

    let script = fs::read_to_string(&script_src)
        .unwrap_or_else(|error| panic!("Failed to read {}: {error}", script_src.display()));
    let scanner = fs::read_to_string(&scanner_src)
        .unwrap_or_else(|error| panic!("Failed to read {}: {error}", scanner_src.display()));
    write_file(&script_dst, &script);
    write_file(&scanner_dst, &scanner);
    write_file(&workflow_path, workflow_content);

    let output = bash_command()
        .arg("scripts/validate-workflow-awk.sh")
        .arg(".github/workflows/test.yml")
        .current_dir(temp_root.path())
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "Failed to run AWK validator in {}: {error}",
                temp_root.path().display()
            )
        });

    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));

    (output.status.success(), combined.replace("\r\n", "\n"))
}

#[derive(Debug)]
struct ValidatorCase {
    name: &'static str,
    workflow: &'static str,
    expected_success: bool,
    must_contain: Vec<&'static str>,
    must_not_contain: Vec<&'static str>,
}

#[test]
fn test_workflow_awk_validator_detects_gawk_option_variants_individually() {
    let cases = [
        ("long_lint", "awk --lint"),
        ("long_traditional", "awk --traditional"),
        ("short_bignum", "gawk -M"),
        ("short_characters_as_bytes", "gawk -b"),
        ("long_debug", "gawk --debug"),
        ("long_gen_pot", "gawk --gen-pot"),
        ("short_debug", "gawk -D"),
        ("short_dump_variables", "gawk -d"),
        ("compact_dump_variables", "gawk -dvars.out"),
        ("short_gen_pot", "gawk -g"),
        ("short_trace", "gawk -I"),
        ("short_no_optimize", "gawk -s"),
        ("short_lint", "gawk -L"),
    ];

    for (name, command) in cases {
        let workflow = format!(
            r#"name: AWK Option {name}
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          {command} '{{ if (match($0, /foo/, captures)) print captures[1] }}' input.txt
"#
        );

        let (success, output) = run_validator_with_workflow(&workflow);
        assert!(
            !success,
            "Case '{name}' should fail for command `{command}`.\nOutput:\n{output}"
        );
        assert!(
            output.contains("Uses match() capture array in AWK")
                && output.contains("FOUND ISSUES:")
                && output.contains("Lines: 10"),
            "Case '{name}' missing expected AWK capture-array diagnostics for `{command}`.\nOutput:\n{output}"
        );
    }
}

#[test]
fn test_workflow_awk_validator_detects_multiline_awk_antipatterns() {
    let cases = [
        ValidatorCase {
            name: "two_argument_match_function_passes",
            workflow: r#"name: AWK POSIX Match
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '
            {
              if (match($0, /foo/)) {
                print $0
              }
            }
          ' input.txt
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "inline_match_capture_array_fails",
            workflow: r#"name: AWK Match Capture
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '
            {
              if (match($0, /foo/, captures)) {
                print captures[1]
              }
            }
          ' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 12",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "absolute_path_awk_match_capture_array_fails",
            workflow: r#"name: AWK Absolute Path
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          /usr/bin/awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "explicit_awk_variant_names_match_capture_array_fail",
            workflow: r#"name: AWK Variant Names
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          gawk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
          mawk '{ if (match($0, /bar/, captures)) print captures[1] }' input.txt
          nawk '{ if (match($0, /baz/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10, 11, 12",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "compact_w_option_match_capture_array_fails",
            workflow: r#"name: AWK Compact W Option
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk -Wposix '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "recognized_no_operand_options_do_not_hide_inline_program",
            workflow: r#"name: AWK No Operand Options
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk --lint '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
          awk --traditional '{ if (match($0, /bar/, captures)) print captures[1] }' input.txt
          gawk -M '{ if (match($0, /baz/, captures)) print captures[1] }' input.txt
          gawk -b '{ if (match($0, /qux/, captures)) print captures[1] }' input.txt
          gawk --debug '{ if (match($0, /debug/, captures)) print captures[1] }' input.txt
          gawk --gen-pot '{ if (match($0, /pot/, captures)) print captures[1] }' input.txt
          gawk -D '{ if (match($0, /debug-short/, captures)) print captures[1] }' input.txt
          gawk -d '{ if (match($0, /dump-short/, captures)) print captures[1] }' input.txt
          gawk -dvars.out '{ if (match($0, /dump-file/, captures)) print captures[1] }' input.txt
          gawk -g '{ if (match($0, /pot-short/, captures)) print captures[1] }' input.txt
          gawk -I '{ if (match($0, /trace/, captures)) print captures[1] }' input.txt
          gawk -s '{ if (match($0, /slow/, captures)) print captures[1] }' input.txt
          gawk -L '{ if (match($0, /lint/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10, 11, 12",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "short_include_option_does_not_hide_inline_program",
            workflow: r#"name: AWK Short Include Option
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          gawk -i inplace '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "long_include_option_does_not_hide_inline_program",
            workflow: r#"name: AWK Long Include Option
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          gawk --include inplace '{ if (match($0, /bar/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "short_load_option_does_not_hide_inline_program",
            workflow: r#"name: AWK Short Load Option
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          gawk -l ext '{ if (match($0, /baz/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "long_load_equals_option_does_not_hide_inline_program",
            workflow: r#"name: AWK Long Load Equals Option
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          gawk --load=ext '{ if (match($0, /qux/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "source_options_scan_program_operands",
            workflow: r#"name: AWK Source Options
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          gawk -e '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
          gawk --source '{ if (match($0, /bar/, captures)) print captures[1] }' input.txt
          gawk --source='{ if (match($0, /baz/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10, 11, 12",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "exec_options_are_program_file_operands",
            workflow: r#"name: AWK Exec File Options
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          gawk -E './match($0, /foo/, captures).awk' input.txt
          gawk --exec './match($0, /bar/, captures).awk' input.txt
          gawk --exec='./match($0, /baz/, captures).awk' input.txt
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "exec_file_operands_scan_nested_command_substitutions",
            workflow: r#"name: AWK Exec File Nested Command Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          gawk -E "$(awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt)" input.txt
          gawk --exec="$(awk '{ if (match($0, /bar/, captures)) print captures[1] }' input.txt)" input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10, 11",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "w_source_options_scan_program_operands",
            workflow: r#"name: AWK W Source Options
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk -W source='{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
          awk -Wsource='{ if (match($0, /baz/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10, 11",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "w_exec_options_are_program_file_operands",
            workflow: r#"name: AWK W Exec File Options
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk -W exec='./match($0, /foo/, captures).awk' input.txt
          awk -Wexec='./match($0, /bar/, captures).awk' input.txt
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "profile_option_does_not_hide_inline_program",
            workflow: r#"name: AWK Profile Option
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          gawk --profile '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "multiline_match_capture_array_fails",
            workflow: r#"name: AWK Multiline Match Capture
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '
            {
              if (match($0,
                /foo/,
                captures)) {
                print captures[1]
              }
            }
          ' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 12",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "command_substitution_match_capture_array_fails",
            workflow: r#"name: AWK Command Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          BAD=$(awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt)
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "separate_assign_option_command_substitution_match_capture_array_fails",
            workflow: r#"name: AWK Assign Option Command Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk -v value="$(awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt)" '{ print value }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "compact_assign_option_command_substitution_match_capture_array_fails",
            workflow: r#"name: AWK Compact Assign Option Command Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk -vvalue=$(awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt) '{ print value }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "long_assign_option_command_substitution_match_capture_array_fails",
            workflow: r#"name: AWK Long Assign Option Command Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk --assign=value="$(awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt)" '{ print value }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "separate_field_separator_command_substitution_match_capture_array_fails",
            workflow: r#"name: AWK Field Separator Command Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk -F "$(awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt)" '{ print $1 }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "compact_field_separator_command_substitution_match_capture_array_fails",
            workflow: r#"name: AWK Compact Field Separator Command Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk -F$(awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt) '{ print $1 }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "long_field_separator_command_substitution_match_capture_array_fails",
            workflow: r#"name: AWK Long Field Separator Command Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk --field-separator="$(awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt)" '{ print $1 }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "separate_file_option_command_substitution_match_capture_array_fails",
            workflow: r#"name: AWK File Option Command Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk -f "$(awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt)" input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "compact_file_option_command_substitution_match_capture_array_fails",
            workflow: r#"name: AWK Compact File Option Command Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk -f$(awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt) input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "long_file_option_command_substitution_match_capture_array_fails",
            workflow: r#"name: AWK Long File Option Command Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk --file="$(awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt)" input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "single_quoted_assign_option_shell_text_is_ignored",
            workflow: r#"name: AWK Single Quoted Assign Shell Text
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk -v 'value=$(awk { if (match($0, /foo/, captures)) print captures[1] })' '{ print value }' input.txt
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "environment_prefixed_awk_match_capture_array_fails",
            workflow: r#"name: AWK Env Prefix
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          LC_ALL=C awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "env_command_prefixed_awk_match_capture_array_fails",
            workflow: r#"name: AWK Env Command Prefix
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          env LC_ALL=C awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "absolute_path_env_prefixed_awk_match_capture_array_fails",
            workflow: r#"name: AWK Absolute Env Command Prefix
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          /usr/bin/env LC_ALL=C awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "env_unset_operand_option_does_not_hide_awk_match_capture_array",
            workflow: r#"name: AWK Env Unset Operand Option
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          env -u LC_ALL awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "env_chdir_operand_option_does_not_hide_awk_match_capture_array",
            workflow: r#"name: AWK Env Chdir Operand Option
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          env -C /tmp awk '{ if (match($0, /bar/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "env_unset_equals_option_does_not_hide_awk_match_capture_array",
            workflow: r#"name: AWK Env Unset Equals Option
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          env --unset=LC_ALL awk '{ if (match($0, /baz/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "env_chdir_equals_option_does_not_hide_awk_match_capture_array",
            workflow: r#"name: AWK Env Chdir Equals Option
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          env --chdir=/tmp awk '{ if (match($0, /qux/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "env_split_string_options_do_not_hide_awk_match_capture_array",
            workflow: r#"name: AWK Env Split String Options
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          /usr/bin/env -S awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
          /usr/bin/env -S "awk" '{ if (match($0, /bar/, captures)) print captures[1] }' input.txt
          /usr/bin/env --split-string=awk '{ if (match($0, /baz/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10, 11, 12",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "env_command_substitution_awk_match_capture_array_fails",
            workflow: r#"name: AWK Env Command Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          BAD=$(env LC_ALL=C awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt)
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "if_awk_match_capture_array_fails",
            workflow: r#"name: AWK If Command
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          if awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt; then
            echo ok
          fi
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "if_not_awk_match_capture_array_fails",
            workflow: r#"name: AWK If Not Command
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          if ! awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt; then
            echo failed
          fi
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "bare_not_awk_match_capture_array_fails",
            workflow: r#"name: AWK Bare Not Command
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          ! awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "grouped_awk_match_capture_array_fails",
            workflow: r#"name: AWK Grouped Command
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          { awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt; }
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "while_awk_match_capture_array_fails",
            workflow: r#"name: AWK While Command
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          while awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt; do
            break
          done
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "then_awk_match_capture_array_fails",
            workflow: r#"name: AWK Then Command
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          if true; then awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt; fi
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "process_substitution_awk_match_capture_array_fails",
            workflow: r#"name: AWK Process Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          diff expected.txt < <(awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt)
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "output_process_substitution_awk_match_capture_array_fails",
            workflow: r#"name: AWK Output Process Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          tee >(awk '{ if (match($0, /foo/, captures)) print captures[1] }') < input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "time_wrapped_awk_match_capture_array_fails",
            workflow: r#"name: AWK Time Wrapper
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          time awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "command_wrapped_awk_match_capture_array_fails",
            workflow: r#"name: AWK Command Wrapper
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          command awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "multiple_match_capture_array_lines_are_reported",
            workflow: r#"name: AWK Multiple Match Captures
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '
            { if (match($0, /foo/, first)) print first[1] }
            { if (match($0, /bar/, second)) print second[1] }
          ' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 11, 12",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "multiline_nul_printf_warns",
            workflow: r#"name: AWK NUL
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '
            {
              printf "%s\0", $0
            }
          ' input.txt
"#,
            expected_success: true,
            must_contain: vec!["Uses \\0 in printf in AWK", "FOUND WARNINGS:"],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "rust_fence_bare_prefix_regex_fails",
            workflow: r#"name: AWK Rust Fence Prefix
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '$0 ~ /^```[Rr]ust/ { print $0 }' docs.md
"#,
            expected_success: false,
            must_contain: vec![
                "Rust fence AWK regex lacks an explicit language-token boundary",
                "Bare prefixes like /^```[Rr]ust/",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "rust_fence_lowercase_bare_prefix_regex_fails",
            workflow: r#"name: AWK Rust Fence Lowercase Prefix
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '$0 ~ /^```rust/ { print $0 }' docs.md
"#,
            expected_success: false,
            must_contain: vec![
                "Rust fence AWK regex lacks an explicit language-token boundary",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "rust_fence_comma_only_regex_fails",
            workflow: r#"name: AWK Rust Fence Comma Only
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '$0 ~ /^```[Rr]ust(,.*)?$/ { print $0 }' docs.md
"#,
            expected_success: false,
            must_contain: vec![
                "Rust fence AWK regex lacks an explicit language-token boundary",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "rust_fence_token_boundary_regex_passes",
            workflow: r#"name: AWK Rust Fence Token Boundary
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '$0 ~ /^```+[Rr]ust([[:space:],]|$)/ { print $0 }' docs.md
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Rust fence AWK regex lacks"],
        },
        ValidatorCase {
            name: "nul_printf_text_in_awk_string_is_ignored",
            workflow: r#"name: AWK NUL String
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '{ print "printf \"%s\\0\", value" }' input.txt
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses \\0 in printf in AWK"],
        },
        ValidatorCase {
            name: "nul_printf_text_in_awk_regex_is_ignored",
            workflow: r#"name: AWK NUL Regex
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '$0 ~ /printf "%s\\0"/ { print $0 }' input.txt
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses \\0 in printf in AWK"],
        },
        ValidatorCase {
            name: "complex_awk_warning_uses_workflow_line_number",
            workflow: r#"name: AWK Complexity
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '
            BEGIN { total = 0 }
            { total += $1 }
            { count += 1 }
            { max = ($1 > max ? $1 : max) }
            { min = (count == 1 || $1 < min ? $1 : min) }
            { values[count] = $1 }
            { names[count] = $2 }
            { buckets[int($1 / 10)] += 1 }
            { seen[$2] = 1 }
            { ordered[count] = $0 }
            { checksum += length($0) }
            { if ($1 > 100) large += 1 }
            { if ($1 < 10) small += 1 }
            { average = total / count }
            { latest = $0 }
            END { print total, count, average, min, max, latest }
          ' input.txt
"#,
            expected_success: true,
            must_contain: vec!["Complex AWK script", "Near line: 10", "FOUND WARNINGS:"],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "complex_command_substitution_awk_warning_uses_workflow_line_number",
            workflow: r#"name: AWK Command Substitution Complexity
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          REPORT=$(awk '
            BEGIN { total = 0 }
            { total += $1 }
            { count += 1 }
            { max = ($1 > max ? $1 : max) }
            { min = (count == 1 || $1 < min ? $1 : min) }
            { values[count] = $1 }
            { names[count] = $2 }
            { buckets[int($1 / 10)] += 1 }
            { seen[$2] = 1 }
            { ordered[count] = $0 }
            { checksum += length($0) }
            { if ($1 > 100) large += 1 }
            { if ($1 < 10) small += 1 }
            { average = total / count }
            { latest = $0 }
            END { print total, count, average, min, max, latest }
          ' input.txt)
"#,
            expected_success: true,
            must_contain: vec!["Complex AWK script", "Near line: 10", "FOUND WARNINGS:"],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "awk_file_invocation_does_not_scan_unrelated_shell_lines",
            workflow: r#"name: AWK File
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk -f parser.awk input.txt
          echo 'match($0, /foo/, captures)'
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "awk_file_invocation_does_not_hide_later_inline_awk",
            workflow: r#"name: AWK File Then Inline
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk -f parser.awk input.txt; awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "awk_file_invocation_does_not_hide_later_command_substitution_inline_awk",
            workflow: r#"name: AWK File Then Command Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk -f parser.awk input.txt; BAD=$(awk '{ if (match($0, /foo/, captures)) print captures[1] }' input.txt)
"#,
            expected_success: false,
            must_contain: vec![
                "Uses match() capture array in AWK",
                "FOUND ISSUES:",
                "Lines: 10",
            ],
            must_not_contain: vec![],
        },
        ValidatorCase {
            name: "same_line_shell_string_after_awk_is_not_scanned",
            workflow: r#"name: AWK Then Shell String
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '{ print $0 }' input.txt; echo 'match($0, /foo/, captures)'
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "awk_string_literal_match_capture_text_is_ignored",
            workflow: r#"name: AWK String Literal
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '{ print "match($0, /foo/, captures)" }' input.txt
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "awk_regex_literal_match_capture_text_is_ignored",
            workflow: r#"name: AWK Regex Literal
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '$0 ~ /match($0, /foo/, captures)/ { print $0 }' input.txt
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "awk_regex_constant_match_capture_text_is_ignored",
            workflow: r#"name: AWK Regex Constant
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk 'BEGIN { print /match($0, \/foo\/, captures)/ }' input.txt
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "awk_regex_constant_after_boolean_operator_is_ignored",
            workflow: r#"name: AWK Boolean Regex Constant
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk 'BEGIN { ok = flag && /match($0, \/foo\/, captures)/ || /match($0, \/bar\/, captures)/ }' input.txt
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "double_quoted_shell_literal_awk_is_ignored",
            workflow: r#"name: AWK Double Quoted Shell Literal
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          echo "awk '{ if (match($0, /foo/, captures)) print captures[1] }'"
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "unquoted_shell_argument_named_awk_is_ignored",
            workflow: r#"name: AWK Argument Word
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          echo awk '{ if (match($0, /foo/, captures)) print captures[1] }'
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "unquoted_shell_argument_named_absolute_awk_is_ignored",
            workflow: r#"name: AWK Absolute Path Argument Word
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          echo /usr/bin/awk '{ if (match($0, /foo/, captures)) print captures[1] }'
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "single_line_double_quoted_awk_does_not_scan_later_shell_lines",
            workflow: r#"name: AWK Double Quote
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk "{ print $0 }" input.txt
          echo 'match($0, /foo/, captures)'
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "quoted_command_substitution_awk_closes",
            workflow: r#"name: AWK Quoted Command Substitution
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          echo "$(awk '{ print $0 }' input.txt)"
          echo 'match($0, /foo/, captures)'
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "single_line_double_quoted_awk_with_escaped_quote_closes",
            workflow: r#"name: AWK Escaped Quote
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk "{ print \"can't\" }" input.txt
          echo 'match($0, /foo/, captures)'
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
        ValidatorCase {
            name: "single_line_command_substitution_awk_with_mixed_quotes_closes",
            workflow: r#"name: AWK Mixed Quotes
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          CARGO_VERSION=$(awk -F '"' '$1 == "version = " { print $2; exit }' Cargo.toml)
          echo 'match($0, /foo/, captures)'
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK", "Complex AWK script"],
        },
        ValidatorCase {
            name: "inline_awk_comment_match_capture_array_is_ignored",
            workflow: r#"name: AWK Inline Comment
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Validate
        run: |
          awk '
            { print $0 } # old code: match($0, /foo/, captures)
          ' input.txt
"#,
            expected_success: true,
            must_contain: vec!["SUCCESS: No AWK anti-patterns found"],
            must_not_contain: vec!["Uses match() capture array in AWK"],
        },
    ];

    for case in cases {
        let (success, output) = run_validator_with_workflow(case.workflow);

        assert_eq!(
            success, case.expected_success,
            "Case '{}' exit status mismatch.\nOutput:\n{}",
            case.name, output
        );

        for needle in case.must_contain {
            assert!(
                output.contains(needle),
                "Case '{}' missing expected output fragment: '{}'\nOutput:\n{}",
                case.name,
                needle,
                output
            );
        }

        for needle in case.must_not_contain {
            assert!(
                !output.contains(needle),
                "Case '{}' contained unexpected output fragment: '{}'\nOutput:\n{}",
                case.name,
                needle,
                output
            );
        }
    }
}
