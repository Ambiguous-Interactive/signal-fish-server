//! Tests for the human-readable catalog generated from standardized SKILL.md metadata.

#![cfg(test)]

mod common;

use common::{bash_command, read_file, repo_root, unique_temp_dir, write_file};

fn skill(name: &str, description: &str, line_ending: &str) -> String {
    [
        "---",
        &format!("name: {name}"),
        &format!("description: {description}"),
        "---",
        "",
        "# Instructions",
        "",
    ]
    .join(line_ending)
}

fn generate_index_for(skills: &[(&str, &str)]) -> String {
    let temp_root = unique_temp_dir("skills-index");
    let root = temp_root.path();
    let script_src = repo_root().join("scripts/generate-skills-index.sh");
    write_file(
        &root.join("scripts/generate-skills-index.sh"),
        &read_file(&script_src),
    );
    for (name, content) in skills {
        write_file(
            &root.join(".agents/skills").join(name).join("SKILL.md"),
            content,
        );
    }

    let output = bash_command()
        .arg("scripts/generate-skills-index.sh")
        .current_dir(root)
        .output()
        .expect("failed to run skills index generator");
    assert!(
        output.status.success(),
        "generator failed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    read_file(&root.join(".agents/skills/index.md"))
}

#[test]
fn test_generator_reads_frontmatter_and_handles_crlf() {
    let alpha = skill("alpha-one", "Handle alpha tasks.", "\r\n");
    let beta = skill("beta-two", "Handle beta tasks.", "\n");
    let index = generate_index_for(&[("alpha-one", &alpha), ("beta-two", &beta)]);

    assert!(index.contains("[`$alpha-one`](./alpha-one/SKILL.md) — Handle alpha tasks."));
    assert!(index.contains("[`$beta-two`](./beta-two/SKILL.md) — Handle beta tasks."));
    assert!(
        !index.contains('\r'),
        "generated catalog must normalize CRLF metadata"
    );
}

#[test]
fn test_generator_orders_packages_by_path() {
    let alpha = skill("alpha", "Alpha.", "\n");
    let zeta = skill("zeta", "Zeta.", "\n");
    let index = generate_index_for(&[("zeta", &zeta), ("alpha", &alpha)]);
    assert!(index.find("$alpha").expect("alpha entry") < index.find("$zeta").expect("zeta entry"));
}

#[test]
fn test_committed_skills_index_is_fresh() {
    let output = bash_command()
        .args(["scripts/generate-skills-index.sh", "--check"])
        .current_dir(repo_root())
        .output()
        .expect("failed to check committed skills index");
    assert!(
        output.status.success(),
        "committed catalog must be fresh.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
