//! Guard against tests that silently swallow failures.
//!
//! The anti-pattern: an error is logged with `tracing::error!(...)` and then the
//! test (or a helper it calls) `return`s instead of failing. The test passes,
//! and a real regression — e.g. "the WebSocket server never became reachable" —
//! slips through CI unnoticed. Test code must fail LOUDLY: `panic!`/`assert!`/
//! `.expect()`, or return `Result` and propagate the error with `?`.
//!
//! This mirrors the AST-scan technique of `tests/async_timeout_policy_scan.rs`
//! (parse with `syn`, walk the tree, report span lines) and is self-tested with
//! flags/allows fixtures so the detector itself cannot silently rot.
//!
//! Scope is deliberately tight to stay robust (zero false positives):
//!   * Only `error!`, `tracing::error!`, and `log::error!` (an explicit *failure*
//!     signal) count as the log. `eprintln!`/`println!`/`warn!` are excluded —
//!     `eprintln!` in particular is the repo's idiom for benign "skipping because
//!     <tool> is unavailable" notices that legitimately precede a `return`.
//!   * Only the silent returns `return;`, `return ();`, and `return Ok(());`
//!     (claiming success/no-op) are flagged — both bare (`error!(..); return;`)
//!     and guarded (`error!(..); if cond { return; }` / `match`-arm return).
//!   * Only TEST context is scanned: files under a `tests/` dir, files whose
//!     stem is `tests` or ends in `_tests`, `#[cfg(test)]` modules, and `#[test]`
//!     /`#[tokio::test]` functions. Production handlers log-and-return as normal
//!     control flow and must NOT be flagged.
//!
//! Known, accepted limitation: a swallow with an unrelated statement *between*
//! the log and the return (`error!(..); other(); return;`) is not flagged — that
//! shape is not idiomatic, and widening to it would require data-flow analysis
//! that risks false positives. The guarded-return case above is covered because
//! it is the common real shape.

use std::fs;
use std::path::{Path, PathBuf};

use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Block, Expr, ImplItem, Item, Macro, Meta, Stmt};

#[test]
fn test_failures_are_not_silently_swallowed() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Same roots as the timeout/try_recv scans: the reference client's sources
    // and tests are held to the identical discipline.
    let scan_roots = [
        root.join("src"),
        root.join("tests"),
        root.join("clients").join("native").join("src"),
        root.join("clients").join("native").join("tests"),
    ];
    assert_scan_roots_exist(&scan_roots);

    let mut violations = Vec::new();
    for scan_root in &scan_roots {
        for path in rust_source_files(scan_root) {
            let content = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            violations.extend(silent_swallow_violations(
                &relative_path_for_display(&root, &path),
                &content,
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "Test code must fail loudly: an error logged with `tracing::error!` must not be followed \
by a silent `return` (that lets CI pass despite the failure). Use `panic!`/`assert!`/`.expect()`, \
or return `Result` and propagate with `?`:\n{}",
        violations.join("\n")
    );
}

#[test]
fn detector_flags_log_then_silent_return_in_test_context() {
    // `#[cfg(test)]` module makes the helper test context even without `#[test]`;
    // both a bare `return;` and a `return Ok(());` claim success after an error.
    let content = r#"
#[cfg(test)]
mod tests {
    async fn helper() {
        match do_thing() {
            Err(e) => {
                tracing::error!("failed: {e}");
                return;
            }
            Ok(v) => v,
        };
    }

    #[tokio::test]
    async fn t() {
        if bad {
            tracing::error!("bad");
            return Ok(());
        }
    }
}
"#;

    let violations = silent_swallow_violations("fixture.rs", content);
    assert_eq!(
        violations.len(),
        2,
        "expected exactly two violations, got {violations:?}"
    );
    assert!(
        violations.contains(
            &"fixture.rs:8: error logged with `tracing::error!` then silently `return`ed"
                .to_string()
        ),
        "should flag the bare `return;` after the error log, got {violations:?}"
    );
    assert!(
        violations.contains(
            &"fixture.rs:18: error logged with `tracing::error!` then silently `return`ed"
                .to_string()
        ),
        "should flag the `return Ok(());` after the error log, got {violations:?}"
    );
}

#[test]
fn detector_flags_guarded_silent_returns() {
    // The guarded shapes: the return is wrapped in an `if`/`match` immediately
    // after the error log. These are the idiomatic real swallows the bare-adjacent
    // rule alone would miss.
    let content = r#"
#[test]
fn guarded_if() {
    tracing::error!("server never came up");
    if should_stop {
        return;
    }
}

#[test]
fn guarded_match() {
    tracing::error!("bad state");
    match recover() {
        _ => return,
    }
}

#[test]
fn log_macro_only() {
    error!("direct error macro");
    return;
}
"#;

    let violations = silent_swallow_violations("fixture.rs", content);
    assert_eq!(
        violations.len(),
        3,
        "guarded-if, guarded-match, and bare `error!` swallows must all be flagged, got {violations:?}"
    );
}

#[test]
fn detector_allows_loud_failures_skip_notices_and_production_code() {
    let content = r#"
// Production (non-test) fn: log-and-return is legitimate control flow.
fn production_handler() {
    if bad {
        tracing::error!("dropping bad input");
        return;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn loud() {
        match thing() {
            Err(e) => {
                tracing::error!("failed: {e}");
                panic!("failed: {e}");
            }
            Ok(_) => {}
        }
    }

    #[test]
    fn skip_notice_uses_eprintln() {
        eprintln!("skipping because the tool is unavailable");
        return;
    }

    #[test]
    fn early_return_without_error_log() {
        if done {
            return;
        }
    }

    #[test]
    fn guarded_panic_is_loud() {
        tracing::error!("bad");
        if should_stop {
            panic!("bad");
        }
    }

    #[test]
    fn unrelated_third_party_error_macro() {
        // A `*::error!` from some other crate that is not a failure log.
        ui::error!("render a red banner");
        return;
    }
}
"#;

    let violations = silent_swallow_violations("fixture.rs", content);
    assert!(
        violations.is_empty(),
        "loud failures, eprintln skip-notices, and production code must be allowed, got {violations:?}"
    );
}

#[test]
fn detector_uses_path_to_decide_test_context() {
    // A plain fn with no `#[test]` attr: under `tests/` it is test context (and
    // flagged); the identical body under `src/` is production (and allowed).
    let content = r#"
fn helper() {
    if bad {
        tracing::error!("x");
        return;
    }
}
"#;

    let under_tests = silent_swallow_violations("tests/some_it.rs", content);
    assert_eq!(
        under_tests,
        vec![
            "tests/some_it.rs:5: error logged with `tracing::error!` then silently `return`ed"
                .to_string()
        ],
        "files under tests/ are entirely test context"
    );

    let under_src = silent_swallow_violations("src/server/handler.rs", content);
    assert!(
        under_src.is_empty(),
        "a production fn under src/ must not be flagged, got {under_src:?}"
    );
}

fn silent_swallow_violations(file: &str, content: &str) -> Vec<String> {
    let parsed = syn::parse_file(content)
        .unwrap_or_else(|error| panic!("failed to parse {file} as Rust syntax: {error}"));
    let mut violations = Vec::new();
    walk_items(&parsed.items, path_is_all_test(file), file, &mut violations);
    violations.sort();
    violations
}

/// Walk items, threading whether we are inside test context. Test context turns
/// on at a `#[cfg(test)]` module, a `#[test]`/`#[tokio::test]` function, or a
/// file that is entirely test code (handled by the caller's seed value).
fn walk_items(items: &[Item], in_test: bool, file: &str, violations: &mut Vec<String>) {
    for item in items {
        match item {
            Item::Mod(item_mod) => {
                let child_in_test = in_test || attrs_have_cfg_test(&item_mod.attrs);
                if let Some((_, inner)) = &item_mod.content {
                    walk_items(inner, child_in_test, file, violations);
                }
            }
            Item::Fn(item_fn) => {
                if in_test || attrs_have_test(&item_fn.attrs) {
                    scan_block(&item_fn.block, file, violations);
                }
            }
            Item::Impl(item_impl) => {
                for impl_item in &item_impl.items {
                    if let ImplItem::Fn(method) = impl_item {
                        if in_test || attrs_have_test(&method.attrs) {
                            scan_block(&method.block, file, violations);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn scan_block(block: &Block, file: &str, violations: &mut Vec<String>) {
    let mut visitor = SwallowVisitor { file, violations };
    visitor.visit_block(block);
}

struct SwallowVisitor<'a> {
    file: &'a str,
    violations: &'a mut Vec<String>,
}

impl Visit<'_> for SwallowVisitor<'_> {
    fn visit_block(&mut self, block: &Block) {
        // The swallow: an error log whose immediately-following sibling statement
        // performs — or directly guards — a silent return. Covering both the bare
        // `tracing::error!(..); return;` AND the guarded `tracing::error!(..);
        // if cond { return; }` / `match .. { _ => return }` shapes, since the
        // guarded form is the most idiomatic real swallow. Deeper nesting is
        // reached by the recursion below (each block's own adjacent pairs).
        for pair in block.stmts.windows(2) {
            if stmt_is_error_log(&pair[0]) && stmt_leads_to_silent_return(&pair[1]) {
                self.violations.push(format!(
                    "{}:{}: error logged with `tracing::error!` then silently `return`ed",
                    self.file,
                    pair[1].span().start().line
                ));
            }
        }
        // Recurse into nested blocks (match arms, if/else, loops, closures,
        // async blocks, nested fns) so the pattern is found at any depth.
        syn::visit::visit_block(self, block);
    }
}

fn path_is_all_test(file: &str) -> bool {
    let path = Path::new(file);
    let in_tests_dir = path.components().any(|c| c.as_os_str() == "tests");
    let stem_is_test = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem == "tests" || stem.ends_with("_tests"));
    in_tests_dir || stem_is_test
}

fn attrs_have_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|seg| seg.ident == "test")
    })
}

fn attrs_have_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && matches!(&attr.meta, Meta::List(list) if list.tokens.to_string().contains("test"))
    })
}

fn stmt_is_error_log(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Macro(stmt_macro) => macro_is_error_log(&stmt_macro.mac),
        Stmt::Expr(Expr::Macro(expr_macro), _) => macro_is_error_log(&expr_macro.mac),
        _ => false,
    }
}

/// True only for `error!`, `tracing::error!`, and `log::error!` — an explicit
/// failure log. An unrelated third-party `*::error!` macro is NOT matched (it
/// might not signal failure), and `error_span!` etc. differ in their final
/// segment, so they are excluded.
fn macro_is_error_log(mac: &Macro) -> bool {
    let segments: Vec<String> = mac
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    match segments.as_slice() {
        [single] => single == "error",
        [prefix, last] => last == "error" && (prefix == "tracing" || prefix == "log"),
        _ => false,
    }
}

/// True when `stmt` performs a silent return, OR is a control-flow statement
/// (`if`/`match`/bare block) whose taken branch *directly* performs one — i.e.
/// the `tracing::error!(..); if cond { return; }` guarded-swallow shape.
fn stmt_leads_to_silent_return(stmt: &Stmt) -> bool {
    if stmt_is_silent_return(stmt) {
        return true;
    }
    match stmt {
        Stmt::Expr(expr, _) => expr_guards_silent_return(expr),
        _ => false,
    }
}

fn expr_guards_silent_return(expr: &Expr) -> bool {
    match expr {
        Expr::If(expr_if) => {
            block_has_direct_silent_return(&expr_if.then_branch)
                || expr_if
                    .else_branch
                    .as_ref()
                    .is_some_and(|(_, else_expr)| expr_guards_silent_return(else_expr))
        }
        Expr::Match(expr_match) => expr_match
            .arms
            .iter()
            .any(|arm| expr_is_or_blocks_silent_return(&arm.body)),
        Expr::Block(block) => block_has_direct_silent_return(&block.block),
        _ => false,
    }
}

/// A match-arm body is either an expression (`_ => return`) or a block
/// (`_ => { .. return; }`); flag a silent return in either form.
fn expr_is_or_blocks_silent_return(expr: &Expr) -> bool {
    match expr {
        Expr::Return(ret) => match &ret.expr {
            None => true,
            Some(inner) => expr_is_unit_or_ok_unit(inner),
        },
        Expr::Block(block) => block_has_direct_silent_return(&block.block),
        _ => false,
    }
}

fn block_has_direct_silent_return(block: &Block) -> bool {
    block.stmts.iter().any(stmt_is_silent_return)
}

/// True for `return;`, `return ();`, and `return Ok(());` — returns that claim
/// success/no-op rather than surfacing the just-logged failure.
fn stmt_is_silent_return(stmt: &Stmt) -> bool {
    let Stmt::Expr(Expr::Return(ret), _) = stmt else {
        return false;
    };
    match &ret.expr {
        None => true,
        Some(expr) => expr_is_unit_or_ok_unit(expr),
    }
}

fn expr_is_unit_or_ok_unit(expr: &Expr) -> bool {
    match expr {
        Expr::Tuple(tuple) => tuple.elems.is_empty(),
        Expr::Paren(paren) => expr_is_unit_or_ok_unit(&paren.expr),
        Expr::Call(call) => {
            let calls_ok = matches!(
                &*call.func,
                Expr::Path(path) if path.path.segments.last().is_some_and(|seg| seg.ident == "Ok")
            );
            calls_ok
                && call.args.len() == 1
                && matches!(&call.args[0], Expr::Tuple(tuple) if tuple.elems.is_empty())
        }
        _ => false,
    }
}

/// `collect_rust_source_files` silently skips missing directories — fine for
/// nested subtrees, but a moved/renamed scan root would silently drop coverage.
/// Assert every root exists before walking (mirrors the other policy scans).
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

fn relative_path_for_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}
