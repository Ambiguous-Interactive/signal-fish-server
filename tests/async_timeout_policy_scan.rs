use std::fs;
use std::path::{Path, PathBuf};

use syn::parse::Parser;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Block, Expr, Macro, Pat, Stmt, Token};

#[test]
fn test_timeout_await_results_are_not_discarded() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();

    // The native reference client (clients/native/) is a standalone package
    // the root gates never compile; the timeout discipline applies to it
    // identically — INCLUDING its tests/ tree (this rule deliberately covers
    // test code, exactly as it covers the root tests/, because a discarded
    // timeout in a harness silently weakens an assertion).
    let scan_roots = [
        root.join("src"),
        root.join("tests"),
        root.join("clients").join("native").join("src"),
        root.join("clients").join("native").join("tests"),
    ];
    assert_scan_roots_exist(&scan_roots);
    for scan_root in scan_roots {
        for path in rust_source_files(&scan_root) {
            let content = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            violations.extend(discarded_timeout_await_violations(
                &relative_path_for_display(&root, &path),
                &content,
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "Do not discard `tokio::time::timeout(...).await` results. \
Use a named result plus assertions for expected silence, or unwrap/assert the timeout, stream, \
and frame result when draining expected messages:\n{}",
        violations.join("\n")
    );
}

#[test]
fn test_try_recv_results_are_not_discarded_in_tests() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();

    // Same scan roots as the timeout rule above: the reference client's
    // sources and tests are held to the identical try_recv discipline.
    let scan_roots = [
        root.join("tests"),
        root.join("src"),
        root.join("clients").join("native").join("src"),
        root.join("clients").join("native").join("tests"),
    ];
    assert_scan_roots_exist(&scan_roots);
    for path in scan_roots
        .iter()
        .flat_map(|scan_root| rust_source_files(scan_root))
    {
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        violations.extend(discarded_try_recv_violations(
            &relative_path_for_display(&root, &path),
            &content,
        ));
    }

    assert!(
        violations.is_empty(),
        "Do not discard `try_recv()` results in tests. Assert the expected message type/content, \
or explicitly assert the empty/disconnected state being tested:\n{}",
        violations.join("\n")
    );
}

#[test]
fn test_timeout_policy_detector_flags_discarded_timeout_awaits() {
    let content = r#"
async fn fixture() {
    let _ = tokio::time::timeout(duration, receiver.next()).await;
    let _timeout_result = timeout(duration, receiver.next()).await;
    let _ = timeout(duration, receiver.next()).await.unwrap();
    timeout(duration, receiver.next()).await;
    let _ = timeout(duration, async { receiver.next().await }).await;
    assert!(timeout(duration, receiver.recv()).await.unwrap_or(None).is_none());
    assert!(matches!(timeout(duration, receiver.recv()).await, Ok(None)));
    if let Ok(Some(_)) = timeout(duration, receiver.recv()).await {}
    while let Ok(Some(_)) = timeout(duration, receiver.recv()).await {}
}
"#;

    let violations = discarded_timeout_await_violations("fixture.rs", content);

    assert_eq!(
        violations,
        vec![
            "fixture.rs:3: discarded `timeout(...).await` result".to_string(),
            "fixture.rs:4: discarded `timeout(...).await` result".to_string(),
            "fixture.rs:5: discarded `timeout(...).await` result".to_string(),
            "fixture.rs:6: discarded `timeout(...).await` result".to_string(),
            "fixture.rs:7: discarded `timeout(...).await` result".to_string(),
            "fixture.rs:8: discarded `timeout(...).await` result".to_string(),
            "fixture.rs:9: discarded `timeout(...).await` result".to_string(),
            "fixture.rs:10: discarded `timeout(...).await` result".to_string(),
            "fixture.rs:11: discarded `timeout(...).await` result".to_string(),
        ]
    );
}

#[test]
fn test_timeout_policy_detector_allows_asserted_timeout_results() {
    let content = r#"
async fn fixture() {
    let result = tokio::time::timeout(duration, receiver.next()).await;
    assert!(result.is_err());

    let message = timeout(duration, receiver.next())
        .await
        .expect("read timed out")
        .expect("stream closed")
        .expect("websocket error");
    assert!(message.is_text());
}
"#;

    let violations = discarded_timeout_await_violations("fixture.rs", content);

    assert!(
        violations.is_empty(),
        "asserted timeout reads should be allowed, got {violations:?}"
    );
}

#[test]
fn test_try_recv_policy_detector_flags_discarded_reads() {
    let content = r#"
fn fixture() {
    let _ = rx.try_recv();
    let _ = rx.try_recv().unwrap();
    rx.try_recv();
    while let Ok(message) = rx.try_recv() {
        println!("{message:?}");
    }
    while rx.try_recv().is_ok() {}
    if rx.try_recv().is_ok() {
        count += 1;
    }
    assert!(rx.try_recv().is_ok());
    assert!(matches!(rx.try_recv(), Ok(_)));
}
"#;

    let violations = discarded_try_recv_violations("fixture.rs", content);

    assert_eq!(
        violations,
        vec![
            "fixture.rs:3: discarded `try_recv()` result".to_string(),
            "fixture.rs:4: discarded `try_recv()` result".to_string(),
            "fixture.rs:5: discarded `try_recv()` result".to_string(),
            "fixture.rs:6: discarded `try_recv()` result".to_string(),
            "fixture.rs:9: discarded `try_recv()` result".to_string(),
            "fixture.rs:10: discarded `try_recv()` result".to_string(),
            "fixture.rs:13: discarded `try_recv()` result".to_string(),
            "fixture.rs:14: discarded `try_recv()` result".to_string(),
        ]
    );
}

#[test]
fn test_try_recv_policy_detector_allows_explicit_assertions() {
    let content = r##"
fn fixture() {
    let message = rx.try_recv().expect("message present");
    match message.as_ref() {
        ServerMessage::RoomJoined(_) => {}
        other => panic!("unexpected message: {other:?}"),
    }

    match rx.try_recv() {
        Ok(message) => panic!("unexpected message: {message:?}"),
        Err(TryRecvError::Empty) => {}
        Err(TryRecvError::Disconnected) => panic!("closed"),
    }
}
"##;

    let violations = discarded_try_recv_violations("fixture.rs", content);

    assert!(
        violations.is_empty(),
        "asserted try_recv reads should be allowed, got {violations:?}"
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

fn relative_path_for_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn discarded_timeout_await_violations(file: &str, content: &str) -> Vec<String> {
    let parsed = syn::parse_file(content)
        .unwrap_or_else(|error| panic!("failed to parse {file} as Rust syntax: {error}"));
    let mut visitor = DiscardedTimeoutAwaitVisitor {
        file,
        violations: Vec::new(),
    };

    visitor.visit_file(&parsed);
    visitor.violations
}

fn discarded_try_recv_violations(file: &str, content: &str) -> Vec<String> {
    let parsed = syn::parse_file(content)
        .unwrap_or_else(|error| panic!("failed to parse {file} as Rust syntax: {error}"));
    let mut visitor = DiscardedTryRecvVisitor {
        file,
        violations: Vec::new(),
    };

    visitor.visit_file(&parsed);
    visitor.violations
}

struct DiscardedTimeoutAwaitVisitor<'a> {
    file: &'a str,
    violations: Vec<String>,
}

struct DiscardedTryRecvVisitor<'a> {
    file: &'a str,
    violations: Vec<String>,
}

impl Visit<'_> for DiscardedTimeoutAwaitVisitor<'_> {
    fn visit_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Local(local) => {
                if pat_discards_value(&local.pat)
                    && local
                        .init
                        .as_ref()
                        .is_some_and(|init| expr_contains_timeout_await(&init.expr))
                {
                    self.record(local.span().start().line);
                }
            }
            Stmt::Expr(expr, Some(_)) => {
                if expr_discards_timeout_result(expr) {
                    self.record(expr.span().start().line);
                }
            }
            Stmt::Macro(macro_stmt) => {
                if macro_contains_timeout_await(&macro_stmt.mac) {
                    self.record(macro_stmt.span().start().line);
                }
            }
            Stmt::Expr(expr, None) => {
                if expr_discards_timeout_control_flow(expr) {
                    self.record(expr.span().start().line);
                }
            }
            Stmt::Item(_) => {}
        }

        syn::visit::visit_stmt(self, stmt);
    }
}

impl DiscardedTimeoutAwaitVisitor<'_> {
    fn record(&mut self, line: usize) {
        self.violations.push(format!(
            "{}:{line}: discarded `timeout(...).await` result",
            self.file
        ));
    }
}

impl Visit<'_> for DiscardedTryRecvVisitor<'_> {
    fn visit_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Local(local) => {
                if pat_discards_value(&local.pat)
                    && local
                        .init
                        .as_ref()
                        .is_some_and(|init| expr_contains_try_recv_call(&init.expr))
                {
                    self.record(local.span().start().line);
                }
            }
            Stmt::Expr(expr, Some(_)) => {
                if expr_discards_try_recv_result(expr) {
                    self.record(expr.span().start().line);
                }
            }
            Stmt::Expr(expr, None) => {
                if expr_discards_try_recv_control_flow(expr) {
                    self.record(expr.span().start().line);
                }
            }
            Stmt::Macro(macro_stmt) => {
                if macro_contains_try_recv_call(&macro_stmt.mac) {
                    self.record(macro_stmt.span().start().line);
                }
            }
            Stmt::Item(_) => {}
        }

        syn::visit::visit_stmt(self, stmt);
    }
}

impl DiscardedTryRecvVisitor<'_> {
    fn record(&mut self, line: usize) {
        self.violations.push(format!(
            "{}:{line}: discarded `try_recv()` result",
            self.file
        ));
    }
}

fn pat_discards_value(pat: &Pat) -> bool {
    match pat {
        Pat::Wild(_) => true,
        Pat::Ident(pat) => pat.ident.to_string().starts_with('_'),
        Pat::Paren(pat) => pat_discards_value(&pat.pat),
        Pat::Type(pat) => pat_discards_value(&pat.pat),
        _ => false,
    }
}

fn expr_contains_timeout_await(expr: &Expr) -> bool {
    match expr {
        Expr::Await(expr) => expr_is_timeout_read_call(&expr.base),
        Expr::Call(call) => {
            expr_contains_timeout_await(&call.func)
                || call.args.iter().any(expr_contains_timeout_await)
        }
        Expr::MethodCall(method_call) => {
            expr_contains_timeout_await(&method_call.receiver)
                || method_call.args.iter().any(expr_contains_timeout_await)
        }
        Expr::Let(expr) => expr_contains_timeout_await(&expr.expr),
        Expr::Group(expr) => expr_contains_timeout_await(&expr.expr),
        Expr::Paren(expr) => expr_contains_timeout_await(&expr.expr),
        Expr::Try(expr) => expr_contains_timeout_await(&expr.expr),
        _ => false,
    }
}

fn expr_is_timeout_read_call(expr: &Expr) -> bool {
    match expr {
        Expr::Call(call) => {
            expr_is_timeout_path(&call.func) && call.args.iter().any(expr_contains_read_future)
        }
        Expr::Group(expr) => expr_is_timeout_read_call(&expr.expr),
        Expr::Paren(expr) => expr_is_timeout_read_call(&expr.expr),
        _ => false,
    }
}

fn expr_is_timeout_path(expr: &Expr) -> bool {
    let Expr::Path(path) = expr else {
        return false;
    };

    path.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "timeout")
}

fn expr_contains_read_future(expr: &Expr) -> bool {
    match expr {
        Expr::MethodCall(method_call) => {
            matches!(method_call.method.to_string().as_str(), "next" | "recv")
                || expr_contains_read_future(&method_call.receiver)
                || method_call.args.iter().any(expr_contains_read_future)
        }
        Expr::Call(call) => {
            expr_contains_read_future(&call.func) || call.args.iter().any(expr_contains_read_future)
        }
        Expr::Await(expr) => expr_contains_read_future(&expr.base),
        Expr::Async(expr) => block_contains_read_future(&expr.block),
        Expr::Group(expr) => expr_contains_read_future(&expr.expr),
        Expr::Paren(expr) => expr_contains_read_future(&expr.expr),
        Expr::Try(expr) => expr_contains_read_future(&expr.expr),
        _ => false,
    }
}

fn expr_discards_timeout_result(expr: &Expr) -> bool {
    match expr {
        Expr::Await(_) => expr_contains_timeout_await(expr),
        Expr::Call(_) | Expr::MethodCall(_) | Expr::Try(_) => expr_contains_timeout_await(expr),
        Expr::If(expr) => expr_contains_timeout_await(&expr.cond),
        Expr::While(expr) => expr_contains_timeout_await(&expr.cond),
        Expr::Macro(expr) => macro_contains_timeout_await(&expr.mac),
        Expr::Group(expr) => expr_discards_timeout_result(&expr.expr),
        Expr::Paren(expr) => expr_discards_timeout_result(&expr.expr),
        _ => false,
    }
}

fn expr_discards_timeout_control_flow(expr: &Expr) -> bool {
    match expr {
        Expr::If(expr) => expr_contains_timeout_await(&expr.cond),
        Expr::While(expr) => expr_contains_timeout_await(&expr.cond),
        Expr::Macro(expr) => macro_contains_timeout_await(&expr.mac),
        Expr::Group(expr) => expr_discards_timeout_control_flow(&expr.expr),
        Expr::Paren(expr) => expr_discards_timeout_control_flow(&expr.expr),
        _ => false,
    }
}

fn expr_contains_try_recv_call(expr: &Expr) -> bool {
    match expr {
        Expr::MethodCall(method_call) => {
            method_call.method == "try_recv"
                || expr_contains_try_recv_call(&method_call.receiver)
                || method_call.args.iter().any(expr_contains_try_recv_call)
        }
        Expr::Call(call) => {
            expr_contains_try_recv_call(&call.func)
                || call.args.iter().any(expr_contains_try_recv_call)
        }
        Expr::Await(expr) => expr_contains_try_recv_call(&expr.base),
        Expr::If(expr) => {
            expr_contains_try_recv_call(&expr.cond)
                || expr
                    .else_branch
                    .as_ref()
                    .is_some_and(|(_, else_expr)| expr_contains_try_recv_call(else_expr))
        }
        Expr::Let(expr) => expr_contains_try_recv_call(&expr.expr),
        Expr::While(expr) => expr_contains_try_recv_call(&expr.cond),
        Expr::Macro(expr) => macro_contains_try_recv_call(&expr.mac),
        Expr::Group(expr) => expr_contains_try_recv_call(&expr.expr),
        Expr::Paren(expr) => expr_contains_try_recv_call(&expr.expr),
        _ => false,
    }
}

fn expr_discards_try_recv_result(expr: &Expr) -> bool {
    match expr {
        Expr::Call(_) | Expr::MethodCall(_) | Expr::Try(_) => expr_contains_try_recv_call(expr),
        Expr::If(expr) => expr_contains_try_recv_call(&expr.cond),
        Expr::While(expr) => expr_contains_try_recv_call(&expr.cond),
        Expr::Macro(expr) => macro_contains_try_recv_call(&expr.mac),
        Expr::Group(expr) => expr_discards_try_recv_result(&expr.expr),
        Expr::Paren(expr) => expr_discards_try_recv_result(&expr.expr),
        _ => false,
    }
}

fn expr_discards_try_recv_control_flow(expr: &Expr) -> bool {
    match expr {
        Expr::If(expr) => expr_contains_try_recv_call(&expr.cond),
        Expr::While(expr) => expr_contains_try_recv_call(&expr.cond),
        Expr::Macro(expr) => macro_contains_try_recv_call(&expr.mac),
        Expr::Group(expr) => expr_discards_try_recv_control_flow(&expr.expr),
        Expr::Paren(expr) => expr_discards_try_recv_control_flow(&expr.expr),
        _ => false,
    }
}

fn block_contains_read_future(block: &Block) -> bool {
    block.stmts.iter().any(|stmt| match stmt {
        Stmt::Local(local) => local
            .init
            .as_ref()
            .is_some_and(|init| expr_contains_read_future(&init.expr)),
        Stmt::Expr(expr, _) => expr_contains_read_future(expr),
        Stmt::Item(_) | Stmt::Macro(_) => false,
    })
}

fn macro_contains_timeout_await(mac: &Macro) -> bool {
    macro_argument_exprs(mac).iter().any(|expr| {
        expr_discards_timeout_result(expr)
            || expr_contains_timeout_await(expr)
            || expr_contains_timeout_await_in_nested_macros(expr)
    })
}

fn macro_contains_try_recv_call(mac: &Macro) -> bool {
    macro_argument_exprs(mac)
        .iter()
        .any(expr_contains_try_recv_call)
}

fn expr_contains_timeout_await_in_nested_macros(expr: &Expr) -> bool {
    match expr {
        Expr::Macro(expr) => macro_contains_timeout_await(&expr.mac),
        Expr::Call(call) => {
            expr_contains_timeout_await_in_nested_macros(&call.func)
                || call
                    .args
                    .iter()
                    .any(expr_contains_timeout_await_in_nested_macros)
        }
        Expr::MethodCall(method_call) => {
            expr_contains_timeout_await_in_nested_macros(&method_call.receiver)
                || method_call
                    .args
                    .iter()
                    .any(expr_contains_timeout_await_in_nested_macros)
        }
        Expr::If(expr) => {
            expr_contains_timeout_await_in_nested_macros(&expr.cond)
                || expr.else_branch.as_ref().is_some_and(|(_, else_expr)| {
                    expr_contains_timeout_await_in_nested_macros(else_expr)
                })
        }
        Expr::Let(expr) => expr_contains_timeout_await_in_nested_macros(&expr.expr),
        Expr::While(expr) => expr_contains_timeout_await_in_nested_macros(&expr.cond),
        Expr::Await(expr) => expr_contains_timeout_await_in_nested_macros(&expr.base),
        Expr::Group(expr) => expr_contains_timeout_await_in_nested_macros(&expr.expr),
        Expr::Paren(expr) => expr_contains_timeout_await_in_nested_macros(&expr.expr),
        Expr::Try(expr) => expr_contains_timeout_await_in_nested_macros(&expr.expr),
        _ => false,
    }
}

fn macro_argument_exprs(mac: &Macro) -> Vec<Expr> {
    let tokens = mac.tokens.clone();
    if let Ok(expr) = syn::parse2::<Expr>(tokens.clone()) {
        return vec![expr];
    }

    let parser = syn::punctuated::Punctuated::<Expr, Token![,]>::parse_terminated;
    if let Ok(exprs) = parser.parse2(tokens) {
        return exprs.into_iter().collect();
    }

    Vec::new()
}
