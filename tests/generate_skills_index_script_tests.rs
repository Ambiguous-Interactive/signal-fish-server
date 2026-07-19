//! Tests for the portable Agent Skills catalog generator and its compatibility wrapper.

#![cfg(test)]

mod common;

use common::{bash_command, read_file, repo_root, unique_temp_dir, write_file};

fn generate_index_for(skills: &[(&str, &str)]) -> std::process::Output {
    let temp_root = unique_temp_dir("skills-index");
    let root = temp_root.path();
    let repository = repo_root();

    for relative in [
        "scripts/generate-skills-index.sh",
        ".llm/skills/manage-skills/scripts/generate_skills_index.py",
        ".llm/skills/manage-skills/scripts/validate_skills.py",
    ] {
        write_file(&root.join(relative), &read_file(&repository.join(relative)));
    }
    write_file(
        &root.join(".llm/skills/manage-skills/SKILL.md"),
        "---\nname: manage-skills\ndescription: >-\n  Manage fixture skills. Use when testing the catalog generator.\n---\n\n# Manage Skills\n\nRun [generate](scripts/generate_skills_index.py) and [validate](scripts/validate_skills.py).\n",
    );
    for (name, content) in skills {
        write_file(
            &root.join(".llm/skills").join(name).join("SKILL.md"),
            content,
        );
    }

    let output = bash_command()
        .arg("scripts/generate-skills-index.sh")
        .current_dir(root)
        .output()
        .unwrap_or_else(|error| panic!("failed to run catalog generator: {error}"));

    if output.status.success() {
        let generated = read_file(&root.join(".llm/skills/index.md"));
        assert!(generated.contains("## Available Skills"));
    }
    output
}

fn fixture(name: &str, title: &str, line_ending: &str) -> String {
    format!(
        "---{0}name: {name}{0}description: >-{0}  Exercise a catalog fixture. Use when testing deterministic skill discovery.{0}---{0}{0}# {title}{0}{0}Fixture body.{0}",
        line_ending
    )
}

#[test]
fn generator_reads_frontmatter_and_handles_crlf() {
    let alpha = fixture("alpha-one", "Alpha One", "\r\n");
    let beta = fixture("beta-two", "Beta Two", "\n");
    let output = generate_index_for(&[("alpha-one", &alpha), ("beta-two", &beta)]);

    assert!(
        output.status.success(),
        "generator failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn generator_rejects_a_nonstandard_skill_entrypoint() {
    let output = generate_index_for(&[("missing-frontmatter", "# Missing Frontmatter\n")]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("must start with YAML frontmatter"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn committed_catalog_has_clean_link_text_and_descriptions() {
    let index = read_file(&repo_root().join(".llm/skills/index.md"));
    assert!(
        !index.contains('\r'),
        "catalog contains a stray carriage return"
    );
    for line in index.lines().filter(|line| line.starts_with("- [")) {
        assert!(
            line.contains("/SKILL.md) (`"),
            "nonstandard catalog link: {line}"
        );
        assert!(
            line.contains(" — "),
            "catalog entry lacks a description: {line}"
        );
    }
}
