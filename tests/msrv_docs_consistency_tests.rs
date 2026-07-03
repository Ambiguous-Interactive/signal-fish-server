use std::{fs, path::PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &str) -> String {
    fs::read_to_string(repo_root().join(path)).expect("failed to read file")
}

fn msrv_from_cargo() -> String {
    let cargo_toml = read("Cargo.toml");
    cargo_toml
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("rust-version = ")
                .and_then(|value| value.trim().strip_prefix('"'))
                .and_then(|value| value.strip_suffix('"'))
                .map(ToOwned::to_owned)
        })
        .expect("Cargo.toml must define rust-version")
}

fn major_minor(version: &str) -> String {
    let mut parts = version.split('.');
    let major = parts.next().expect("missing major");
    let minor = parts.next().expect("missing minor");
    format!("{major}.{minor}")
}

#[test]
fn docs_development_mentions_exact_msrv() {
    let msrv = msrv_from_cargo();
    let development = read("docs/development.md");
    assert!(
        development.contains(&format!("Rust {msrv} or later")),
        "docs/development.md must mention exact MSRV from Cargo.toml ({msrv})"
    );
    assert!(
        development.contains(&format!("rust-version = \"{msrv}\"")),
        "docs/development.md must include rust-version = \"{msrv}\" in MSRV section"
    );
}

#[test]
fn docs_quickstart_mentions_msrv_major_minor() {
    let msrv = msrv_from_cargo();
    let quickstart = read("docs/quickstart.md");
    let short = major_minor(&msrv);
    assert!(
        quickstart.contains(&format!("Rust {short}+")),
        "docs/quickstart.md must mention Rust {short}+ based on Cargo.toml rust-version {msrv}"
    );
}
