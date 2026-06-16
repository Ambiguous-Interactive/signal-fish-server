//! Tests for `scripts/generate-skills-index.sh` — the `.llm/skills/index.md`
//! generator.
//!
//! The generator extracts each skill's title from its `# Skill: ...` heading and
//! emits a markdown link `- [Title](./file.md)`. Skill files are frequently
//! checked out with CRLF line endings (the repo's `.gitattributes` only pins LF
//! for `*.sh`/`*.ps1`/`.githooks/*`, not `*.md`), so the heading line can end with
//! a carriage return. If the generator does not strip that `\r`, the emitted link
//! text becomes `[Title\r](./file.md)` — a STRAY CR INSIDE the link text. That is
//! mid-line content (not a line ending), so Git's EOL normalization never removes
//! it, and markdownlint rejects it as `MD039/no-space-in-links`. Regenerating the
//! index in a CRLF working tree would then silently produce a committed index that
//! fails the markdown lint. These tests lock the generator against that.

#![cfg(test)]

mod common;

use common::{bash_command, read_file, repo_root, unique_temp_dir, write_file};

/// Run the REAL `scripts/generate-skills-index.sh` in a hermetic temp "repo"
/// containing only the script + the given skill fixtures, and return the
/// generated `index.md` content.
///
/// The script derives its repo root from its own path (`$BASH_SOURCE/..`), so
/// copying it to `<temp>/scripts/` and running it from `<temp>` makes it operate
/// on `<temp>/.llm/skills/` — fully isolated from the real repo.
fn generate_index_for(skills: &[(&str, &str)]) -> String {
    let temp_root = unique_temp_dir("skills-index");
    let root = temp_root.path();

    let script_src = repo_root().join("scripts/generate-skills-index.sh");
    write_file(
        &root.join("scripts/generate-skills-index.sh"),
        &read_file(&script_src),
    );

    for (file_name, content) in skills {
        write_file(&root.join(".llm/skills").join(file_name), content);
    }

    let output = bash_command()
        .arg("scripts/generate-skills-index.sh")
        .current_dir(root)
        .output()
        .unwrap_or_else(|e| panic!("Failed to run generate-skills-index.sh: {e}"));

    assert!(
        output.status.success(),
        "generate-skills-index.sh exited non-zero.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    read_file(&root.join(".llm/skills/index.md"))
}

/// Assert no generated link has trailing whitespace/CR in its link text — the
/// `[Title ](...)` / `[Title\r](...)` shape markdownlint flags as MD039.
fn assert_no_trailing_space_in_link_text(index: &str, origin: &str) {
    assert!(
        !index.contains('\r'),
        "{origin} contains a carriage return — a CRLF skill heading leaked `\\r` into the index \
         link text (markdownlint MD039). Regenerate with scripts/generate-skills-index.sh.\n\
         Raw:\n{index:?}"
    );
    for (line_no, line) in index.lines().enumerate() {
        if let Some(pos) = line.find("](") {
            let link_text = &line[..pos];
            assert!(
                !link_text.ends_with(char::is_whitespace),
                "{origin} line {} has trailing whitespace inside the link text (markdownlint \
                 MD039 no-space-in-links): {line:?}\n\
                 The generator must strip trailing whitespace/CR from the `# Skill:` title.",
                line_no + 1
            );
        }
    }
}

#[test]
fn test_generator_strips_cr_from_crlf_skill_headings() {
    // A CRLF-checked-out skill (`\r\n` line endings) must NOT leak a trailing
    // carriage return into the index link text. This is the regression: with the
    // pre-fix generator, `alpha.md`'s title became `Alpha One\r`, emitting
    // `[Alpha One\r](./alpha.md)` (MD039). The LF skill must of course also work.
    let index = generate_index_for(&[
        ("alpha.md", "# Skill: Alpha One\r\n\r\nCRLF body\r\n"),
        ("beta.md", "# Skill: Beta Two\n\nLF body\n"),
    ]);

    assert_no_trailing_space_in_link_text(&index, "generated index (CRLF fixture)");

    assert!(
        index.contains("[Alpha One](./alpha.md)"),
        "CRLF skill title must be emitted clean as `[Alpha One](./alpha.md)`, got:\n{index}"
    );
    assert!(
        index.contains("[Beta Two](./beta.md)"),
        "LF skill title must be emitted as `[Beta Two](./beta.md)`, got:\n{index}"
    );
}

#[test]
fn test_generator_falls_back_to_filename_without_skill_heading() {
    // A file lacking a `# Skill:` heading falls back to a filename link — and that
    // fallback must also be clean (no CR even if the file is CRLF).
    let index = generate_index_for(&[("no-heading.md", "Some intro line\r\n\r\nbody\r\n")]);
    assert_no_trailing_space_in_link_text(&index, "generated index (no-heading fixture)");
    assert!(
        index.contains("[no-heading.md](./no-heading.md)"),
        "headingless skill must fall back to a clean filename link, got:\n{index}"
    );
}

#[test]
fn test_committed_skills_index_has_clean_link_text() {
    // Guard the REAL committed index: it must never carry a stray CR / trailing
    // space in link text (which would fail markdownlint MD039 in CI). If this
    // fails, the index was regenerated in a CRLF working tree with a generator
    // that does not strip the CR — regenerate after pulling the fix.
    let index = read_file(&repo_root().join(".llm/skills/index.md"));
    assert_no_trailing_space_in_link_text(&index, ".llm/skills/index.md");
}
