#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;

const VALID_SKILL: &str = "---\nname: ci-guide\ndescription: Diagnose CI failures and workflow configuration. Use for CI errors and GitHub Actions changes.\n---\n\n# CI Guide\n\nDiagnose the failure from evidence.\n";
const VALID_METADATA: &str = "interface:\n  display_name: \"CI Guide\"\n  short_description: \"Diagnose CI workflow failures\"\n  default_prompt: \"Use $ci-guide to diagnose this CI failure.\"\n";

fn run_checker_with_fixture(files: &[(&str, &str)], args: &[&str]) -> (i32, String) {
    let temp_root = unique_temp_dir("agent-skill-validation");
    let script_src = repo_root().join("scripts/validate-agent-skills.sh");
    let script_dst = temp_root.path().join("scripts/validate-agent-skills.sh");
    let script = fs::read_to_string(&script_src)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", script_src.display()));
    write_file(&script_dst, &script);

    for (relative_path, content) in files {
        write_file(&temp_root.path().join(relative_path), content);
    }

    let output = bash_command()
        .arg("scripts/validate-agent-skills.sh")
        .args(args)
        .current_dir(temp_root.path())
        .output()
        .unwrap_or_else(|e| panic!("Failed to run Agent Skills validator: {e}"));
    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (
        output.status.code().unwrap_or(-1),
        combined.replace("\r\n", "\n"),
    )
}

#[derive(Debug)]
struct ScriptCase {
    name: &'static str,
    files: Vec<(&'static str, &'static str)>,
    args: Vec<&'static str>,
    expected_exit: i32,
    must_contain: Vec<&'static str>,
}

#[test]
fn test_agent_skill_validator_data_driven_cases() {
    let cases = vec![
        ScriptCase {
            name: "accepts_standard_package",
            files: vec![
                (".agents/skills/ci-guide/SKILL.md", VALID_SKILL),
                (".agents/skills/ci-guide/agents/openai.yaml", VALID_METADATA),
            ],
            args: vec![],
            expected_exit: 0,
            must_contain: vec![
                "[INFO] Validated 1 Agent Skills package(s)",
                "[OK] Agent Skills structure and routing are valid",
            ],
        },
        ScriptCase {
            name: "rejects_extra_frontmatter_key",
            files: vec![
                (
                    ".agents/skills/ci-guide/SKILL.md",
                    "---\nname: ci-guide\ndescription: Diagnose CI failures. Use for CI errors.\nversion: 1\n---\n\n# CI Guide\n",
                ),
                (".agents/skills/ci-guide/agents/openai.yaml", VALID_METADATA),
            ],
            args: vec![],
            expected_exit: 1,
            must_contain: vec!["frontmatter must contain exactly name and description"],
        },
        ScriptCase {
            name: "rejects_unrouted_reference",
            files: vec![
                (".agents/skills/ci-guide/SKILL.md", VALID_SKILL),
                (".agents/skills/ci-guide/agents/openai.yaml", VALID_METADATA),
                (".agents/skills/ci-guide/references/cache.md", "# Cache\n"),
            ],
            args: vec![],
            expected_exit: 1,
            must_contain: vec!["does not directly route reference cache.md"],
        },
        ScriptCase {
            name: "files_mode_validates_owning_package",
            files: vec![
                (".agents/skills/ci-guide/SKILL.md", VALID_SKILL),
                (".agents/skills/ci-guide/agents/openai.yaml", VALID_METADATA),
            ],
            args: vec!["--files", ".agents/skills/ci-guide/agents/openai.yaml"],
            expected_exit: 0,
            must_contain: vec!["Validated 1 Agent Skills package(s)"],
        },
        ScriptCase {
            name: "files_mode_requires_arguments",
            files: vec![],
            args: vec!["--files"],
            expected_exit: 2,
            must_contain: vec!["[ERROR] --files requires at least one path"],
        },
    ];

    for case in cases {
        let (exit_code, output) = run_checker_with_fixture(&case.files, &case.args);
        assert_eq!(
            exit_code, case.expected_exit,
            "Case '{}' exit mismatch.\nOutput:\n{}",
            case.name, output
        );
        for needle in case.must_contain {
            assert!(
                output.contains(needle),
                "Case '{}' missing expected fragment '{needle}'.\nOutput:\n{output}",
                case.name
            );
        }
    }
}

#[test]
fn test_repository_agent_skills_validate() {
    let output = bash_command()
        .arg("scripts/validate-agent-skills.sh")
        .current_dir(repo_root())
        .output()
        .expect("failed to run repository Agent Skills validator");
    assert!(
        output.status.success(),
        "Repository Agent Skills must validate.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
