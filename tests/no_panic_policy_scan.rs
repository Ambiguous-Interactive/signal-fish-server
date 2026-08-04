use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, Span, TokenStream, TokenTree};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Attribute, Block, Expr, Item, Macro, Meta, Token, UseTree};

const FORBIDDEN_PRODUCTION_MACROS: &[&str] = &[
    "panic",
    "todo",
    "unimplemented",
    "unreachable",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
];
const FORBIDDEN_PRODUCTION_FUNCTIONS: &[&str] = &["panic_any", "resume_unwind"];

#[test]
fn test_rust_production_panic_patterns_are_absent() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();

    // The native reference client (clients/native/) is a standalone package
    // the root gates never compile, so this scan walks its sources too: the
    // production-code panic policy applies to the client crate identically.
    // Out-of-line files are exempted only when every discovered module
    // inclusion is guarded by a test-only cfg; test-like filenames alone do
    // not weaken production coverage.
    let scan_roots = [
        root.join("src"),
        root.join("clients").join("native").join("src"),
    ];
    assert_scan_roots_exist(&scan_roots);
    let test_only_files = verified_out_of_line_test_modules(&scan_roots);
    for scan_root in scan_roots {
        for path in rust_source_files(&scan_root) {
            let normalized = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if test_only_files.contains(&normalized) {
                continue;
            }

            let content = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            violations.extend(production_panic_pattern_violations(
                &relative_path_for_display(&root, &path),
                &content,
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "Production Rust code must not contain panic-prone macros:\n{}",
        violations.join("\n")
    );
}

#[test]
fn test_rust_panic_policy_detector_uses_rust_syntax() {
    let content = r#"
#[cfg(test)]
mod tests {
    const OPEN: &str = "{";

    fn helper() {
        panic!("test panic");
    }
}

pub fn production() {
    panic!("prod");
}

#[cfg(all(test, feature = "fixture"))]
fn test_only_with_feature() {
    unreachable!("test-only");
}

#[cfg(any(test, feature = "fixture"))]
fn can_compile_in_production() {
    todo!("prod");
}
"#;

    let violations = production_panic_pattern_violations("fixture.rs", content);
    assert_eq!(
        violations,
        vec![
            "fixture.rs:12: production `panic!` macro".to_string(),
            "fixture.rs:22: production `todo!` macro".to_string(),
        ]
    );
}

#[test]
fn test_rust_panic_policy_detector_scans_macro_rules_transcribers() {
    let content = r#"
macro_rules! prod_fail {
    () => {
        panic!("boom");
    };
}
"#;

    let violations = production_panic_pattern_violations("fixture.rs", content);
    assert_eq!(
        violations,
        vec!["fixture.rs:4: production `panic!` macro".to_string()]
    );
}

#[test]
fn test_rust_panic_policy_detector_scans_ordinary_macro_inputs() {
    let content = r#"
pub fn production() {
    vec![panic!("boom")];
    format!("{}", todo!("later"));
}
"#;

    let violations = production_panic_pattern_violations("fixture.rs", content);
    assert_eq!(
        violations,
        vec![
            "fixture.rs:3: production `panic!` macro".to_string(),
            "fixture.rs:4: production `todo!` macro".to_string(),
        ]
    );
}

#[test]
fn test_rust_panic_policy_detector_scans_assertions_in_macro_fallback() {
    let content = r#"
macro_rules! prod_check {
    ($extra:expr) => {
        $extra;
        debug_assert_eq!(1, 2);
    };
}
"#;

    let violations = production_panic_pattern_violations("fixture.rs", content);
    assert_eq!(
        violations,
        vec!["fixture.rs:5: production `debug_assert_eq!` macro".to_string()]
    );
}

#[test]
fn test_rust_panic_policy_detector_rejects_assertion_macros() {
    let content = r#"
pub fn production(left: u64, right: u64) {
    assert!(left > 0);
    assert_eq!(left, right);
    assert_ne!(left, 0);
    debug_assert!(right > 0);
    debug_assert_eq!(left, right);
    debug_assert_ne!(right, 0);
}
"#;

    let violations = production_panic_pattern_violations("fixture.rs", content);
    assert_eq!(
        violations,
        vec![
            "fixture.rs:3: production `assert!` macro".to_string(),
            "fixture.rs:4: production `assert_eq!` macro".to_string(),
            "fixture.rs:5: production `assert_ne!` macro".to_string(),
            "fixture.rs:6: production `debug_assert!` macro".to_string(),
            "fixture.rs:7: production `debug_assert_eq!` macro".to_string(),
            "fixture.rs:8: production `debug_assert_ne!` macro".to_string(),
        ]
    );
}

#[test]
fn test_rust_panic_policy_detector_rejects_macro_aliases_and_lint_suppressions() {
    let content = r#"
use std::assert as invariant;

#[allow(clippy::unwrap_used)]
pub fn production() {
    invariant!(false);
}
"#;

    let violations = production_panic_pattern_violations("fixture.rs", content);
    assert_eq!(
        violations.len(),
        2,
        "both bypasses must be rejected: {violations:?}"
    );
    assert!(violations
        .iter()
        .any(|violation| violation.contains("alias/import")));
    assert!(violations
        .iter()
        .any(|violation| violation.contains("lint suppression")));
}

#[test]
fn test_rust_panic_policy_detector_rejects_conditional_and_reasoned_suppressions() {
    let content = r#"
#[cfg_attr(not(test), allow(clippy::unwrap_used))]
pub fn conditional() {
    let _ = None::<u8>.unwrap();
}

#[allow(clippy::expect_used, reason = "fixture invariant")]
pub fn reasoned() {
    let _ = None::<u8>.expect("missing");
}
"#;

    let violations = production_panic_pattern_violations("fixture.rs", content);
    assert_eq!(
        violations
            .iter()
            .filter(|violation| violation.contains("lint suppression"))
            .count(),
        2,
        "conditional and reasoned lint suppressions must both be rejected: {violations:?}"
    );
}

#[test]
fn test_rust_panic_policy_detector_rejects_generated_lint_suppressions() {
    let content = r#"
macro_rules! suppress_generated_item {
    ($item:item) => {
        #[allow(clippy::unwrap_used)]
        $item
    };
}
"#;

    let violations = production_panic_pattern_violations("fixture.rs", content);
    assert!(
        violations
            .iter()
            .any(|violation| violation.contains("lint suppression")),
        "metavariable macro transcribers must not hide lint suppressions: {violations:?}"
    );
}

#[test]
fn test_rust_panic_policy_detector_rejects_direct_panic_functions() {
    let content = r#"
pub fn production() {
    std::panic::panic_any("boom");
}
"#;
    let violations = production_panic_pattern_violations("fixture.rs", content);
    assert_eq!(
        violations,
        vec!["fixture.rs:3: production call to forbidden `panic_any` function".to_string()]
    );
}

#[test]
fn test_rust_panic_policy_detector_allows_test_only_assertion_macros() {
    let content = r#"
mod production_parent {
    #[cfg(test)]
    mod tests {
        #[test]
        fn assertions_are_test_failures() {
            assert!(true);
            assert_eq!(1, 1);
            assert_ne!(1, 2);
            debug_assert!(true);
            debug_assert_eq!(1, 1);
            debug_assert_ne!(1, 2);
        }
    }
}
"#;

    let violations = production_panic_pattern_violations("fixture.rs", content);
    assert!(
        violations.is_empty(),
        "test-only assertion macros must remain allowed: {violations:?}"
    );
}

#[test]
fn test_rust_panic_policy_detector_honors_test_only_match_arms_and_field_values() {
    let content = r#"
struct Fixture {
    value: u64,
}

pub fn production(value: u64) {
    match value {
        #[cfg(test)]
        _ => panic!("test only"),
        _ => {}
    }

    let _fixture = Fixture {
        #[cfg(test)]
        value: unreachable!("test only"),
        #[cfg(not(test))]
        value,
    };
}
"#;

    let violations = production_panic_pattern_violations("fixture.rs", content);
    assert!(
        violations.is_empty(),
        "test-only attrs on match arms and field values must suppress violations: {violations:?}"
    );
}

#[test]
fn test_rust_panic_policy_detector_honors_nested_test_only_attrs() {
    let content = r#"
struct Fixture;

impl Fixture {
    #[cfg(test)]
    fn helper() {
        panic!("test only");
    }
}

trait Contract {
    #[cfg(test)]
    fn helper() {
        panic!("test only");
    }
}

pub fn prod() {
    #[cfg(test)]
    let _value = panic!("test only");

    #[cfg(test)]
    panic!("test only");
}
"#;

    let violations = production_panic_pattern_violations("fixture.rs", content);
    assert!(
        violations.is_empty(),
        "test-only attrs on impl items, trait items, locals, and statement macros must suppress violations: {violations:?}"
    );
}

#[test]
fn test_rust_panic_policy_detector_honors_test_only_attrs_in_macro_fallback() {
    let content = r#"
macro_rules! prod_macro {
    ($extra:expr) => {
        #[cfg(test)]
        panic!("test only");
        #[test]
        fn generated_sync_test() {
            unreachable!("test only");
        }
        #[tokio::test]
        async fn generated_async_test() {
            todo!("test only");
        }
        $extra
    };
}
"#;

    let violations = production_panic_pattern_violations("fixture.rs", content);
    assert!(
        violations.is_empty(),
        "test-only attrs in metavariable-containing macro transcribers must suppress violations: {violations:?}"
    );
}

#[test]
fn test_rust_panic_policy_detector_does_not_over_skip_an_attributed_metavariable() {
    let content = r#"
macro_rules! generated_items {
    ($item:item) => {
        #[cfg(test)]
        $item
        fn production() {
            assert!(false);
        }
    };
}
"#;

    let violations = production_panic_pattern_violations("fixture.rs", content);
    assert_eq!(
        violations,
        vec!["fixture.rs:7: production `assert!` macro".to_string()]
    );
}

/// `collect_rust_source_files` silently skips missing directories — fine for
/// nested subtrees, but a moved/renamed scan root (e.g. `clients/native`)
/// would silently drop policy coverage. Assert every root exists before
/// walking.
fn assert_scan_roots_exist(scan_roots: &[PathBuf]) {
    for scan_root in scan_roots {
        assert!(
            scan_root.exists(),
            "scan root {} is missing — update the policy scans if the directory moved",
            scan_root.display()
        );
    }
}

fn rust_source_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rust_source_files(dir, &mut files);
    files.sort();
    files
}

fn collect_rust_source_files(dir: &Path, files: &mut Vec<PathBuf>) {
    if !dir.exists() {
        return;
    }

    for entry in fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()))
    {
        let entry = entry.unwrap_or_else(|error| panic!("failed to read directory entry: {error}"));
        let path = entry.path();
        if path.is_dir() {
            collect_rust_source_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn verified_out_of_line_test_modules(scan_roots: &[PathBuf]) -> HashSet<PathBuf> {
    let mut test_only = HashSet::new();
    let mut production = HashSet::new();
    let mut unresolved_production_includes = Vec::new();
    for scan_root in scan_roots {
        for declaring_file in rust_source_files(scan_root) {
            let Ok(content) = fs::read_to_string(&declaring_file) else {
                continue;
            };
            let Ok(parsed) = syn::parse_file(&content) else {
                continue;
            };
            collect_module_inclusions(
                &parsed.items,
                &declaring_file,
                false,
                &mut test_only,
                &mut production,
            );
            let mut macro_inclusions = MacroInclusionVisitor {
                source_file: declaring_file.clone(),
                module_file: declaring_file.clone(),
                inherited_test_only: false,
                test_only: &mut test_only,
                production: &mut production,
                unresolved_production_includes: &mut unresolved_production_includes,
            };
            macro_inclusions.visit_file(&parsed);
        }
    }
    assert!(
        unresolved_production_includes.is_empty(),
        "production source inclusions must use direct, unaliased include!(\"literal\") paths so the panic-policy scanner can resolve them:\n{}",
        unresolved_production_includes.join("\n")
    );
    test_only.retain(|path| !production.contains(path));
    test_only
}

fn collect_module_inclusions(
    items: &[Item],
    declaring_file: &Path,
    inherited_test_only: bool,
    test_only: &mut HashSet<PathBuf>,
    production: &mut HashSet<PathBuf>,
) {
    for item in items {
        let Item::Mod(module) = item else {
            continue;
        };
        let module_is_test_only = inherited_test_only || attrs_are_test_only(&module.attrs);
        if let Some((_, nested)) = &module.content {
            let virtual_parent =
                module_file_candidates(declaring_file, &module.ident.to_string())[1].clone();
            collect_module_inclusions(
                nested,
                &virtual_parent,
                module_is_test_only,
                test_only,
                production,
            );
            continue;
        }

        for candidate in module_inclusion_candidates(declaring_file, module, module_is_test_only) {
            if !candidate.is_file() {
                continue;
            }
            let candidate = fs::canonicalize(&candidate).unwrap_or(candidate);
            if module_is_test_only {
                test_only.insert(candidate);
            } else {
                production.insert(candidate);
            }
        }
    }
}

struct MacroInclusionVisitor<'sets> {
    source_file: PathBuf,
    module_file: PathBuf,
    inherited_test_only: bool,
    test_only: &'sets mut HashSet<PathBuf>,
    production: &'sets mut HashSet<PathBuf>,
    unresolved_production_includes: &'sets mut Vec<String>,
}

impl MacroInclusionVisitor<'_> {
    fn visit_with_attrs(&mut self, attrs: &[Attribute], visit: impl FnOnce(&mut Self)) {
        let previous = self.inherited_test_only;
        self.inherited_test_only |= attrs_are_test_only(attrs);
        visit(self);
        self.inherited_test_only = previous;
    }

    fn record_module_path(&mut self, relative_path: &str) {
        let parent = self.module_file.parent().unwrap_or_else(|| Path::new(""));
        self.record_candidate(parent.join(relative_path));
    }

    fn record_include_path(&mut self, relative_path: &str) {
        let parent = self.source_file.parent().unwrap_or_else(|| Path::new(""));
        self.record_candidate(parent.join(relative_path));
    }

    fn record_candidate(&mut self, candidate: PathBuf) {
        if !candidate.is_file() {
            return;
        }
        let candidate = fs::canonicalize(&candidate).unwrap_or(candidate);
        if self.inherited_test_only {
            self.test_only.insert(candidate);
        } else {
            self.production.insert(candidate);
        }
    }

    fn record_unresolved_include(&mut self) {
        if !self.inherited_test_only {
            self.unresolved_production_includes
                .push(self.source_file.display().to_string());
        }
    }

    fn record_macro_inclusion_tokens(&mut self, stream: &TokenStream) {
        let tokens = stream.clone().into_iter().collect::<Vec<_>>();
        let mut idx = 0;
        while idx < tokens.len() {
            if let Some((meta, _, next)) = macro_attr_at(&tokens, idx) {
                if meta_is_test_only_attr(&meta) {
                    idx = macro_attr_target_end(&tokens, next);
                    continue;
                }

                let mut overrides = Vec::new();
                let mut always_applies = false;
                collect_module_path_overrides(
                    &meta,
                    self.inherited_test_only,
                    true,
                    &mut overrides,
                    &mut always_applies,
                );
                for path in overrides {
                    self.record_module_path(&path);
                }
                idx = next;
                continue;
            }

            match &tokens[idx] {
                TokenTree::Ident(ident)
                    if ident == "include" && token_tree_is_bang(tokens.get(idx + 1)) =>
                {
                    if let Some(TokenTree::Group(group)) = tokens.get(idx + 2) {
                        if let Ok(path) = syn::parse2::<syn::LitStr>(group.stream()) {
                            self.record_include_path(&path.value());
                        } else {
                            self.record_unresolved_include();
                        }
                        idx += 3;
                        continue;
                    }
                }
                TokenTree::Ident(ident) if ident == "include" => {
                    // Macro transcribers can import and rename `include!`.
                    // Name resolution depends on the eventual expansion site,
                    // so any non-invocation occurrence fails closed.
                    self.record_unresolved_include();
                }
                TokenTree::Group(group) => {
                    self.record_macro_inclusion_tokens(&group.stream());
                }
                _ => {}
            }
            idx += 1;
        }
    }
}

impl<'ast> Visit<'ast> for MacroInclusionVisitor<'_> {
    fn visit_item(&mut self, node: &'ast Item) {
        let previous_test_only = self.inherited_test_only;
        self.inherited_test_only |= attrs_are_test_only(item_attrs(node));
        let previous_file = self.module_file.clone();
        if let Item::Mod(module) = node {
            if module.content.is_some() {
                self.module_file =
                    module_file_candidates(&previous_file, &module.ident.to_string())[1].clone();
            }
        }
        syn::visit::visit_item(self, node);
        self.module_file = previous_file;
        self.inherited_test_only = previous_test_only;
    }

    fn visit_expr(&mut self, node: &'ast Expr) {
        self.visit_with_attrs(expr_attrs(node), |visitor| {
            syn::visit::visit_expr(visitor, node);
        });
    }

    fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
        self.visit_with_attrs(impl_item_attrs(node), |visitor| {
            syn::visit::visit_impl_item(visitor, node);
        });
    }

    fn visit_trait_item(&mut self, node: &'ast syn::TraitItem) {
        self.visit_with_attrs(trait_item_attrs(node), |visitor| {
            syn::visit::visit_trait_item(visitor, node);
        });
    }

    fn visit_local(&mut self, node: &'ast syn::Local) {
        self.visit_with_attrs(&node.attrs, |visitor| {
            syn::visit::visit_local(visitor, node);
        });
    }

    fn visit_stmt_macro(&mut self, node: &'ast syn::StmtMacro) {
        self.visit_with_attrs(&node.attrs, |visitor| visitor.visit_macro(&node.mac));
    }

    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        self.visit_with_attrs(&node.attrs, |visitor| {
            syn::visit::visit_arm(visitor, node);
        });
    }

    fn visit_field_value(&mut self, node: &'ast syn::FieldValue) {
        self.visit_with_attrs(&node.attrs, |visitor| {
            syn::visit::visit_field_value(visitor, node);
        });
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        if use_tree_imports_name(&node.tree, "include") {
            self.record_unresolved_include();
        }
        syn::visit::visit_item_use(self, node);
    }

    fn visit_macro(&mut self, node: &'ast Macro) {
        if node
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "include")
        {
            if let Ok(path) = syn::parse2::<syn::LitStr>(node.tokens.clone()) {
                self.record_include_path(&path.value());
            } else {
                self.record_unresolved_include();
            }
        } else {
            self.record_macro_inclusion_tokens(&node.tokens);
        }
    }
}

fn use_tree_imports_name(tree: &UseTree, expected: &str) -> bool {
    match tree {
        UseTree::Path(path) => use_tree_imports_name(&path.tree, expected),
        UseTree::Name(name) => name.ident == expected,
        UseTree::Rename(rename) => rename.ident == expected,
        UseTree::Group(group) => group
            .items
            .iter()
            .any(|item| use_tree_imports_name(item, expected)),
        UseTree::Glob(_) => false,
    }
}

fn module_inclusion_candidates(
    declaring_file: &Path,
    module: &syn::ItemMod,
    test_context: bool,
) -> Vec<PathBuf> {
    let parent = declaring_file.parent().unwrap_or_else(|| Path::new(""));
    let mut overrides = Vec::new();
    let mut override_always_applies = false;
    for attr in &module.attrs {
        collect_module_path_overrides(
            &attr.meta,
            test_context,
            true,
            &mut overrides,
            &mut override_always_applies,
        );
    }

    let mut candidates = overrides
        .into_iter()
        .map(|path| parent.join(path))
        .collect::<Vec<_>>();
    if !override_always_applies {
        candidates.extend(module_file_candidates(
            declaring_file,
            &module.ident.to_string(),
        ));
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn collect_module_path_overrides(
    meta: &Meta,
    test_context: bool,
    enclosing_always_applies: bool,
    overrides: &mut Vec<String>,
    override_always_applies: &mut bool,
) {
    if let Meta::NameValue(name_value) = meta {
        if name_value.path.is_ident("path") {
            if let Expr::Lit(expr) = &name_value.value {
                if let syn::Lit::Str(path) = &expr.lit {
                    overrides.push(path.value());
                    *override_always_applies |= enclosing_always_applies;
                }
            }
        }
        return;
    }

    let Meta::List(list) = meta else {
        return;
    };
    if !list.path.is_ident("cfg_attr") {
        return;
    }
    let Some(items) = cfg_list_items(list) else {
        return;
    };
    let Some((condition, nested_attrs)) = items.split_first() else {
        return;
    };
    let (may_be_false, may_be_true) = cfg_meta_truth_values(condition, test_context);
    if !may_be_true {
        return;
    }
    for nested in nested_attrs {
        collect_module_path_overrides(
            nested,
            test_context,
            enclosing_always_applies && !may_be_false,
            overrides,
            override_always_applies,
        );
    }
}

fn module_file_candidates(declaring_file: &Path, module_name: &str) -> [PathBuf; 2] {
    let parent = declaring_file.parent().unwrap_or_else(|| Path::new(""));
    let stem = declaring_file.file_stem().and_then(|stem| stem.to_str());
    let module_dir = if matches!(stem, Some("lib" | "main" | "mod")) {
        parent.to_path_buf()
    } else {
        parent.join(stem.unwrap_or_default())
    };
    [
        module_dir.join(format!("{module_name}.rs")),
        module_dir.join(module_name).join("mod.rs"),
    ]
}

#[test]
fn test_test_like_filename_requires_a_verified_cfg_test_module_declaration() {
    let unverified = PathBuf::from("/repo/src/live_tests.rs");
    let verified = HashSet::<PathBuf>::new();
    assert!(!verified.contains(&unverified));

    let candidates = module_file_candidates(Path::new("/repo/src/lib.rs"), "live_tests");
    assert_eq!(candidates[0], unverified);
}

#[test]
fn test_path_override_can_mark_a_test_named_file_as_a_production_inclusion() {
    let module: syn::ItemMod = syn::parse_str(
        r#"#[path = "foo_tests.rs"]
mod production;"#,
    )
    .expect("fixture module parses");
    let candidates = module_inclusion_candidates(Path::new("/repo/src/lib.rs"), &module, false);
    assert_eq!(candidates, vec![PathBuf::from("/repo/src/foo_tests.rs")]);
    assert!(!attrs_are_test_only(&module.attrs));
}

#[test]
fn test_cfg_attr_path_override_cannot_exempt_a_production_inclusion() {
    let test_module: syn::ItemMod = syn::parse_str(
        r#"#[cfg(test)]
mod foo_tests;"#,
    )
    .expect("test fixture module parses");
    let production_module: syn::ItemMod = syn::parse_str(
        r#"#[cfg(not(test))]
#[cfg_attr(not(test), path = "foo_tests.rs")]
mod production;"#,
    )
    .expect("production fixture module parses");

    let declaring_file = Path::new("/repo/src/lib.rs");
    let test_candidates = module_inclusion_candidates(declaring_file, &test_module, true);
    let production_candidates =
        module_inclusion_candidates(declaring_file, &production_module, false);
    let mut test_only = test_candidates.into_iter().collect::<HashSet<_>>();
    let production = production_candidates.into_iter().collect::<HashSet<_>>();
    test_only.retain(|path| !production.contains(path));

    let shared_file = PathBuf::from("/repo/src/foo_tests.rs");
    assert!(production.contains(&shared_file));
    assert!(
        !test_only.contains(&shared_file),
        "a file included by any production module must not remain test-only"
    );
}

#[test]
fn test_include_and_generated_path_prevent_false_test_only_exemption() {
    let temp = tempfile::tempdir().expect("temporary fixture directory is created");
    let declaring_file = temp.path().join("lib.rs");
    let shared_file = temp.path().join("foo_tests.rs");
    fs::write(&shared_file, "assert!(true);").expect("shared fixture file is written");
    let content = r#"
#[cfg(test)]
mod foo_tests;

mod production {
    include!("foo_tests.rs");
}

macro_rules! generated_production_module {
    () => {
        #[path = "foo_tests.rs"]
        mod generated;
    };
}
"#;
    let parsed = syn::parse_file(content).expect("fixture source parses");
    let mut test_only = HashSet::new();
    let mut production = HashSet::new();
    collect_module_inclusions(
        &parsed.items,
        &declaring_file,
        false,
        &mut test_only,
        &mut production,
    );
    let mut unresolved_production_includes = Vec::new();
    let mut macro_inclusions = MacroInclusionVisitor {
        source_file: declaring_file.clone(),
        module_file: declaring_file.clone(),
        inherited_test_only: false,
        test_only: &mut test_only,
        production: &mut production,
        unresolved_production_includes: &mut unresolved_production_includes,
    };
    macro_inclusions.visit_file(&parsed);
    test_only.retain(|path| !production.contains(path));

    let shared_file = fs::canonicalize(shared_file).expect("shared fixture path canonicalizes");
    assert!(production.contains(&shared_file));
    assert!(!test_only.contains(&shared_file));
    assert!(unresolved_production_includes.is_empty());
}

#[test]
fn test_non_literal_production_include_fails_closed() {
    let declaring_file = PathBuf::from("/repo/src/lib.rs");
    let parsed = syn::parse_file(
        r#"
#[cfg(test)]
mod foo_tests;

mod production {
    include!(concat!("foo_", "tests.rs"));
}
"#,
    )
    .expect("fixture source parses");
    let mut test_only = HashSet::new();
    let mut production = HashSet::new();
    let mut unresolved_production_includes = Vec::new();
    let mut macro_inclusions = MacroInclusionVisitor {
        source_file: declaring_file.clone(),
        module_file: declaring_file,
        inherited_test_only: false,
        test_only: &mut test_only,
        production: &mut production,
        unresolved_production_includes: &mut unresolved_production_includes,
    };
    macro_inclusions.visit_file(&parsed);

    assert_eq!(
        unresolved_production_includes,
        vec!["/repo/src/lib.rs".to_string()]
    );
}

#[test]
fn test_qualified_production_include_prevents_test_only_exemption() {
    let temp = tempfile::tempdir().expect("temporary fixture directory is created");
    let declaring_file = temp.path().join("lib.rs");
    let shared_file = temp.path().join("foo_tests.rs");
    fs::write(&shared_file, "assert!(true);").expect("shared fixture file is written");
    let parsed = syn::parse_file(
        r#"
#[cfg(test)]
mod foo_tests;

mod production {
    std::include!("foo_tests.rs");
}
"#,
    )
    .expect("fixture source parses");
    let mut test_only = HashSet::new();
    let mut production = HashSet::new();
    collect_module_inclusions(
        &parsed.items,
        &declaring_file,
        false,
        &mut test_only,
        &mut production,
    );
    let mut unresolved_production_includes = Vec::new();
    let mut macro_inclusions = MacroInclusionVisitor {
        source_file: declaring_file.clone(),
        module_file: declaring_file,
        inherited_test_only: false,
        test_only: &mut test_only,
        production: &mut production,
        unresolved_production_includes: &mut unresolved_production_includes,
    };
    macro_inclusions.visit_file(&parsed);
    test_only.retain(|path| !production.contains(path));

    let shared_file = fs::canonicalize(shared_file).expect("shared fixture path canonicalizes");
    assert!(production.contains(&shared_file));
    assert!(!test_only.contains(&shared_file));
    assert!(unresolved_production_includes.is_empty());
}

#[test]
fn test_aliased_production_include_fails_closed() {
    let declaring_file = PathBuf::from("/repo/src/lib.rs");
    let parsed = syn::parse_file(
        r#"
use std::include as embed;

mod production {
    embed!("foo_tests.rs");
}
"#,
    )
    .expect("fixture source parses");
    let mut test_only = HashSet::new();
    let mut production = HashSet::new();
    let mut unresolved_production_includes = Vec::new();
    let mut macro_inclusions = MacroInclusionVisitor {
        source_file: declaring_file.clone(),
        module_file: declaring_file,
        inherited_test_only: false,
        test_only: &mut test_only,
        production: &mut production,
        unresolved_production_includes: &mut unresolved_production_includes,
    };
    macro_inclusions.visit_file(&parsed);

    assert_eq!(
        unresolved_production_includes,
        vec!["/repo/src/lib.rs".to_string()]
    );
}

#[test]
fn test_generated_path_inside_inline_module_uses_virtual_module_directory() {
    let temp = tempfile::tempdir().expect("temporary fixture directory is created");
    let declaring_file = temp.path().join("lib.rs");
    let outer = temp.path().join("outer");
    fs::create_dir(&outer).expect("inline module fixture directory is created");
    let shared_file = outer.join("foo_tests.rs");
    fs::write(&shared_file, "assert!(true);").expect("shared fixture file is written");
    let parsed = syn::parse_file(
        r#"
#[cfg(test)]
#[path = "outer/foo_tests.rs"]
mod foo_tests;

mod outer {
    macro_rules! generate {
        () => {
            #[path = "foo_tests.rs"]
            mod production;
        };
    }
}
"#,
    )
    .expect("fixture source parses");
    let mut test_only = HashSet::new();
    let mut production = HashSet::new();
    collect_module_inclusions(
        &parsed.items,
        &declaring_file,
        false,
        &mut test_only,
        &mut production,
    );
    let mut unresolved_production_includes = Vec::new();
    let mut macro_inclusions = MacroInclusionVisitor {
        source_file: declaring_file.clone(),
        module_file: declaring_file,
        inherited_test_only: false,
        test_only: &mut test_only,
        production: &mut production,
        unresolved_production_includes: &mut unresolved_production_includes,
    };
    macro_inclusions.visit_file(&parsed);
    test_only.retain(|path| !production.contains(path));

    let shared_file = fs::canonicalize(shared_file).expect("shared fixture path canonicalizes");
    assert!(production.contains(&shared_file));
    assert!(!test_only.contains(&shared_file));
    assert!(unresolved_production_includes.is_empty());
}

fn relative_path_for_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn production_panic_pattern_violations(file: &str, content: &str) -> Vec<String> {
    let parsed = syn::parse_file(content)
        .unwrap_or_else(|error| panic!("failed to parse Rust source {file}: {error}"));
    let mut visitor = PanicPolicyVisitor {
        file,
        violations: Vec::new(),
    };
    visitor.visit_file(&parsed);
    visitor.violations
}

struct PanicPolicyVisitor<'file> {
    file: &'file str,
    violations: Vec<String>,
}

impl<'ast> Visit<'ast> for PanicPolicyVisitor<'_> {
    fn visit_item(&mut self, node: &'ast Item) {
        if attrs_are_test_only(item_attrs(node)) {
            return;
        }

        syn::visit::visit_item(self, node);
    }

    fn visit_expr(&mut self, node: &'ast Expr) {
        if attrs_are_test_only(expr_attrs(node)) {
            return;
        }

        syn::visit::visit_expr(self, node);
    }

    fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
        if attrs_are_test_only(impl_item_attrs(node)) {
            return;
        }

        syn::visit::visit_impl_item(self, node);
    }

    fn visit_trait_item(&mut self, node: &'ast syn::TraitItem) {
        if attrs_are_test_only(trait_item_attrs(node)) {
            return;
        }

        syn::visit::visit_trait_item(self, node);
    }

    fn visit_local(&mut self, node: &'ast syn::Local) {
        if attrs_are_test_only(&node.attrs) {
            return;
        }

        syn::visit::visit_local(self, node);
    }

    fn visit_stmt_macro(&mut self, node: &'ast syn::StmtMacro) {
        if attrs_are_test_only(&node.attrs) {
            return;
        }

        self.visit_macro(&node.mac);
    }

    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        if attrs_are_test_only(&node.attrs) {
            return;
        }

        syn::visit::visit_arm(self, node);
    }

    fn visit_field_value(&mut self, node: &'ast syn::FieldValue) {
        if attrs_are_test_only(&node.attrs) {
            return;
        }

        syn::visit::visit_field_value(self, node);
    }

    fn visit_macro(&mut self, node: &'ast Macro) {
        if macro_path_is_macro_rules(&node.path) {
            for transcriber in macro_rules_transcriber_groups(&node.tokens) {
                self.visit_macro_transcriber_group(&transcriber);
            }
            return;
        }

        if let Some(macro_name) = forbidden_macro_name(node) {
            self.violations.push(format!(
                "{}:{}: production `{macro_name}!` macro",
                self.file,
                line_for_span(node.path.span())
            ));
        }

        self.record_forbidden_macro_tokens(&node.tokens);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        if attrs_are_test_only(&node.attrs) {
            return;
        }
        let mut imported = Vec::new();
        forbidden_imported_macro_names(&node.tree, &mut imported);
        for macro_name in imported {
            self.violations.push(format!(
                "{}:{}: production alias/import of forbidden `{macro_name}!` macro",
                self.file,
                line_for_span(node.span())
            ));
        }
        syn::visit::visit_item_use(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if attrs_are_test_only(&node.attrs) {
            return;
        }
        if let Expr::Path(path) = node.func.as_ref() {
            if let Some(name) = path
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string())
            {
                if FORBIDDEN_PRODUCTION_FUNCTIONS.contains(&name.as_str()) {
                    self.violations.push(format!(
                        "{}:{}: production call to forbidden `{name}` function",
                        self.file,
                        line_for_span(node.span())
                    ));
                }
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_attribute(&mut self, node: &'ast Attribute) {
        if meta_has_production_panic_lint_suppression(&node.meta) {
            self.violations.push(format!(
                "{}:{}: production panic-policy lint suppression",
                self.file,
                line_for_span(node.span())
            ));
        }
        syn::visit::visit_attribute(self, node);
    }
}

fn forbidden_imported_macro_names(tree: &UseTree, names: &mut Vec<&'static str>) {
    match tree {
        UseTree::Path(path) => forbidden_imported_macro_names(&path.tree, names),
        UseTree::Name(name) => push_forbidden_import_name(&name.ident.to_string(), names),
        UseTree::Rename(rename) => push_forbidden_import_name(&rename.ident.to_string(), names),
        UseTree::Group(group) => {
            for item in &group.items {
                forbidden_imported_macro_names(item, names);
            }
        }
        UseTree::Glob(_) => {}
    }
}

fn push_forbidden_import_name(name: &str, names: &mut Vec<&'static str>) {
    if let Some(forbidden) = FORBIDDEN_PRODUCTION_MACROS
        .iter()
        .chain(FORBIDDEN_PRODUCTION_FUNCTIONS.iter())
        .copied()
        .find(|forbidden| *forbidden == name)
    {
        names.push(forbidden);
    }
}

fn lint_suppression_is_panic_related(tokens: &TokenStream) -> bool {
    let forbidden = [
        "panic",
        "unwrap_used",
        "expect_used",
        "todo",
        "unimplemented",
        "unreachable",
        "indexing_slicing",
        "string_slice",
        "arithmetic_side_effects",
        "restriction",
        "warnings",
        "all",
    ];
    Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(tokens.clone())
        .ok()
        .is_some_and(|items| {
            items.iter().any(|meta| {
                meta.path()
                    .segments
                    .last()
                    .is_some_and(|segment| forbidden.contains(&segment.ident.to_string().as_str()))
            })
        })
}

fn meta_has_production_panic_lint_suppression(meta: &Meta) -> bool {
    let Meta::List(list) = meta else {
        return false;
    };
    if list.path.is_ident("allow") || list.path.is_ident("expect") {
        return lint_suppression_is_panic_related(&list.tokens);
    }
    if !list.path.is_ident("cfg_attr") {
        return false;
    }

    let Some(items) = cfg_list_items(list) else {
        return false;
    };
    let Some((condition, nested_attrs)) = items.split_first() else {
        return false;
    };
    cfg_meta_truth_values(condition, false).1
        && nested_attrs
            .iter()
            .any(meta_has_production_panic_lint_suppression)
}

impl PanicPolicyVisitor<'_> {
    fn visit_macro_transcriber_group(&mut self, group: &proc_macro2::Group) {
        if let Some(block) = parse_token_group_as_block(group) {
            self.visit_block(&block);
            return;
        }

        if let Some(expr) = parse_token_group_as_expr(group) {
            self.visit_expr(&expr);
            return;
        }

        if let Some(block) = parse_token_stream_as_wrapped_block(group.stream()) {
            self.visit_block(&block);
            return;
        }

        self.record_forbidden_macro_tokens(&group.stream());
    }

    fn record_forbidden_macro_tokens(&mut self, stream: &TokenStream) {
        let tokens = stream.clone().into_iter().collect::<Vec<_>>();
        let mut idx = 0;
        while idx < tokens.len() {
            if let Some((meta, span, next)) = macro_attr_at(&tokens, idx) {
                if meta_has_production_panic_lint_suppression(&meta) {
                    self.violations.push(format!(
                        "{}:{}: production panic-policy lint suppression",
                        self.file,
                        line_for_span(span)
                    ));
                }
                idx = if meta_is_test_only_attr(&meta) {
                    macro_attr_target_end(&tokens, next)
                } else {
                    next
                };
                continue;
            }

            match &tokens[idx] {
                TokenTree::Ident(ident)
                    if token_tree_is_bang(tokens.get(idx + 1))
                        && FORBIDDEN_PRODUCTION_MACROS.contains(&ident.to_string().as_str()) =>
                {
                    let macro_name = ident.to_string();
                    self.violations.push(format!(
                        "{}:{}: production `{macro_name}!` macro",
                        self.file,
                        line_for_span(ident.span())
                    ));
                    idx += 2;
                    continue;
                }
                TokenTree::Group(group) => {
                    self.record_forbidden_macro_tokens(&group.stream());
                }
                _ => {}
            }
            idx += 1;
        }
    }
}

fn forbidden_macro_name(node: &Macro) -> Option<&'static str> {
    let macro_name = node.path.segments.last()?.ident.to_string();
    FORBIDDEN_PRODUCTION_MACROS
        .iter()
        .copied()
        .find(|forbidden| *forbidden == macro_name)
}

fn macro_path_is_macro_rules(path: &syn::Path) -> bool {
    path.is_ident("macro_rules")
}

fn macro_rules_transcriber_groups(tokens: &TokenStream) -> Vec<proc_macro2::Group> {
    let token_trees = tokens.clone().into_iter().collect::<Vec<_>>();
    let mut transcribers = Vec::new();
    let mut idx = 0;

    while idx + 2 < token_trees.len() {
        if token_pair_is_fat_arrow(&token_trees, idx) {
            if let Some(TokenTree::Group(group)) = token_trees.get(idx + 2) {
                transcribers.push(group.clone());
                idx += 3;
                continue;
            }
        }
        idx += 1;
    }

    transcribers
}

fn parse_token_group_as_block(group: &proc_macro2::Group) -> Option<Block> {
    if group.delimiter() != Delimiter::Brace {
        return None;
    }
    let stream = group.stream();
    if token_stream_contains_dollar(&stream) {
        return None;
    }

    syn::parse2::<Block>(TokenStream::from(TokenTree::Group(group.clone()))).ok()
}

fn parse_token_group_as_expr(group: &proc_macro2::Group) -> Option<Expr> {
    let stream = group.stream();
    if token_stream_contains_dollar(&stream) {
        return None;
    }

    syn::parse2::<Expr>(TokenStream::from(TokenTree::Group(group.clone()))).ok()
}

fn parse_token_stream_as_wrapped_block(stream: TokenStream) -> Option<Block> {
    if token_stream_contains_dollar(&stream) {
        return None;
    }

    syn::parse2::<Block>(TokenStream::from(TokenTree::Group(
        proc_macro2::Group::new(Delimiter::Brace, stream),
    )))
    .ok()
}

fn token_stream_contains_dollar(stream: &TokenStream) -> bool {
    stream
        .clone()
        .into_iter()
        .any(|token_tree| match token_tree {
            TokenTree::Punct(punct) => punct.as_char() == '$',
            TokenTree::Group(group) => token_stream_contains_dollar(&group.stream()),
            _ => false,
        })
}

fn token_pair_is_fat_arrow(tokens: &[TokenTree], start: usize) -> bool {
    matches!(tokens.get(start), Some(TokenTree::Punct(punct)) if punct.as_char() == '=')
        && matches!(tokens.get(start + 1), Some(TokenTree::Punct(punct)) if punct.as_char() == '>')
}

fn token_tree_is_bang(token_tree: Option<&TokenTree>) -> bool {
    matches!(token_tree, Some(TokenTree::Punct(punct)) if punct.as_char() == '!')
}

fn macro_attr_at(tokens: &[TokenTree], start: usize) -> Option<(Meta, Span, usize)> {
    if !matches!(tokens.get(start), Some(TokenTree::Punct(punct)) if punct.as_char() == '#') {
        return None;
    }

    let Some(TokenTree::Group(group)) = tokens.get(start + 1) else {
        return None;
    };
    if group.delimiter() != Delimiter::Bracket {
        return None;
    }

    let meta = syn::parse2::<Meta>(group.stream()).ok()?;
    Some((meta, group.span(), start + 2))
}

fn macro_attr_target_end(tokens: &[TokenTree], start: usize) -> usize {
    if start >= tokens.len() {
        return start;
    }

    if matches!(tokens.get(start), Some(TokenTree::Punct(punct)) if punct.as_char() == '$') {
        match tokens.get(start + 1) {
            Some(TokenTree::Ident(_)) => return start + 2,
            Some(TokenTree::Group(_)) => {
                let mut end = start + 2;
                if matches!(tokens.get(end), Some(TokenTree::Punct(punct)) if matches!(punct.as_char(), '*' | '+' | '?'))
                {
                    return end + 1;
                }
                if matches!(tokens.get(end + 1), Some(TokenTree::Punct(punct)) if matches!(punct.as_char(), '*' | '+' | '?'))
                {
                    end += 2;
                }
                return end;
            }
            _ => {}
        }
    }

    if macro_token_starts_macro_invocation(tokens, start) {
        return if matches!(tokens.get(start + 2), Some(TokenTree::Group(_))) {
            start + 3
        } else {
            start + 2
        };
    }

    let mut idx = start;
    while idx < tokens.len() {
        match tokens.get(idx) {
            Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Brace => {
                idx += 1;
                if matches!(tokens.get(idx), Some(TokenTree::Ident(ident)) if ident == "else") {
                    idx += 1;
                    continue;
                }
                return idx;
            }
            Some(TokenTree::Punct(punct)) if punct.as_char() == ';' => return idx + 1,
            Some(TokenTree::Punct(punct)) if punct.as_char() == ',' => return idx,
            _ => idx += 1,
        }
    }

    tokens.len()
}

fn macro_token_starts_macro_invocation(tokens: &[TokenTree], start: usize) -> bool {
    matches!(tokens.get(start), Some(TokenTree::Ident(_)))
        && token_tree_is_bang(tokens.get(start + 1))
}

fn meta_is_test_only_attr(meta: &Meta) -> bool {
    if meta.path().is_ident("test") {
        return true;
    }

    if meta
        .path()
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "test")
        && meta.path().segments.len() > 1
    {
        return true;
    }

    match meta {
        Meta::List(list) if list.path.is_ident("cfg") => cfg_list_items(list)
            .is_some_and(|items| matches!(items.as_slice(), [item] if cfg_meta_is_test_only(item))),
        _ => false,
    }
}

fn attrs_are_test_only(attrs: &[Attribute]) -> bool {
    attrs.iter().any(attr_is_test_only)
}

fn attr_is_test_only(attr: &Attribute) -> bool {
    if attr.path().is_ident("test") {
        return true;
    }

    if attr
        .path()
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "test")
        && attr.path().segments.len() > 1
    {
        return true;
    }

    match &attr.meta {
        Meta::List(list) if list.path.is_ident("cfg") => cfg_list_items(list)
            .is_some_and(|items| matches!(items.as_slice(), [item] if cfg_meta_is_test_only(item))),
        _ => false,
    }
}

fn cfg_meta_is_test_only(meta: &Meta) -> bool {
    match meta {
        Meta::Path(path) => path.is_ident("test"),
        Meta::List(list) if list.path.is_ident("all") => {
            cfg_list_items(list).is_some_and(|items| items.iter().any(cfg_meta_is_test_only))
        }
        Meta::List(list) if list.path.is_ident("any") => cfg_list_items(list)
            .is_some_and(|items| !items.is_empty() && items.iter().all(cfg_meta_is_test_only)),
        _ => false,
    }
}

/// Returns `(may_be_false, may_be_true)` for a cfg predicate after fixing the
/// built-in `test` flag. Other predicates remain unknown because features and
/// target cfgs vary between supported production builds.
fn cfg_meta_truth_values(meta: &Meta, test: bool) -> (bool, bool) {
    match meta {
        Meta::Path(path) if path.is_ident("test") => (!test, test),
        Meta::List(list) if list.path.is_ident("not") => cfg_list_items(list)
            .and_then(|items| match items.as_slice() {
                [item] => Some(cfg_meta_truth_values(item, test)),
                _ => None,
            })
            .map_or((true, true), |(may_be_false, may_be_true)| {
                (may_be_true, may_be_false)
            }),
        Meta::List(list) if list.path.is_ident("all") => {
            cfg_list_items(list).map_or((true, true), |items| {
                let values = items
                    .iter()
                    .map(|item| cfg_meta_truth_values(item, test))
                    .collect::<Vec<_>>();
                (
                    values.iter().any(|(may_be_false, _)| *may_be_false),
                    values.iter().all(|(_, may_be_true)| *may_be_true),
                )
            })
        }
        Meta::List(list) if list.path.is_ident("any") => {
            cfg_list_items(list).map_or((true, true), |items| {
                let values = items
                    .iter()
                    .map(|item| cfg_meta_truth_values(item, test))
                    .collect::<Vec<_>>();
                (
                    values.iter().all(|(may_be_false, _)| *may_be_false),
                    values.iter().any(|(_, may_be_true)| *may_be_true),
                )
            })
        }
        _ => (true, true),
    }
}

fn cfg_list_items(list: &syn::MetaList) -> Option<Vec<Meta>> {
    Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(list.tokens.clone())
        .ok()
        .map(|items| items.into_iter().collect())
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

fn impl_item_attrs(item: &syn::ImplItem) -> &[Attribute] {
    match item {
        syn::ImplItem::Const(item) => &item.attrs,
        syn::ImplItem::Fn(item) => &item.attrs,
        syn::ImplItem::Macro(item) => &item.attrs,
        syn::ImplItem::Type(item) => &item.attrs,
        _ => &[],
    }
}

fn trait_item_attrs(item: &syn::TraitItem) -> &[Attribute] {
    match item {
        syn::TraitItem::Const(item) => &item.attrs,
        syn::TraitItem::Fn(item) => &item.attrs,
        syn::TraitItem::Macro(item) => &item.attrs,
        syn::TraitItem::Type(item) => &item.attrs,
        _ => &[],
    }
}

fn expr_attrs(expr: &Expr) -> &[Attribute] {
    match expr {
        Expr::Array(expr) => &expr.attrs,
        Expr::Assign(expr) => &expr.attrs,
        Expr::Async(expr) => &expr.attrs,
        Expr::Await(expr) => &expr.attrs,
        Expr::Binary(expr) => &expr.attrs,
        Expr::Block(expr) => &expr.attrs,
        Expr::Break(expr) => &expr.attrs,
        Expr::Call(expr) => &expr.attrs,
        Expr::Cast(expr) => &expr.attrs,
        Expr::Closure(expr) => &expr.attrs,
        Expr::Const(expr) => &expr.attrs,
        Expr::Continue(expr) => &expr.attrs,
        Expr::Field(expr) => &expr.attrs,
        Expr::ForLoop(expr) => &expr.attrs,
        Expr::Group(expr) => &expr.attrs,
        Expr::If(expr) => &expr.attrs,
        Expr::Index(expr) => &expr.attrs,
        Expr::Infer(expr) => &expr.attrs,
        Expr::Let(expr) => &expr.attrs,
        Expr::Lit(expr) => &expr.attrs,
        Expr::Loop(expr) => &expr.attrs,
        Expr::Macro(expr) => &expr.attrs,
        Expr::Match(expr) => &expr.attrs,
        Expr::MethodCall(expr) => &expr.attrs,
        Expr::Paren(expr) => &expr.attrs,
        Expr::Path(expr) => &expr.attrs,
        Expr::Range(expr) => &expr.attrs,
        Expr::RawAddr(expr) => &expr.attrs,
        Expr::Reference(expr) => &expr.attrs,
        Expr::Repeat(expr) => &expr.attrs,
        Expr::Return(expr) => &expr.attrs,
        Expr::Struct(expr) => &expr.attrs,
        Expr::Try(expr) => &expr.attrs,
        Expr::TryBlock(expr) => &expr.attrs,
        Expr::Tuple(expr) => &expr.attrs,
        Expr::Unary(expr) => &expr.attrs,
        Expr::Unsafe(expr) => &expr.attrs,
        Expr::While(expr) => &expr.attrs,
        Expr::Yield(expr) => &expr.attrs,
        _ => &[],
    }
}

fn line_for_span(span: Span) -> usize {
    let start = span.start();
    if start.line == 0 {
        1
    } else {
        start.line
    }
}
