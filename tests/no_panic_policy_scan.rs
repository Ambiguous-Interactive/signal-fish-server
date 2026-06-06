use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, Span, TokenStream, TokenTree};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Attribute, Block, Expr, Item, Macro, Meta, Token};

const FORBIDDEN_PRODUCTION_MACROS: &[&str] = &["panic", "todo", "unimplemented", "unreachable"];

#[test]
fn test_rust_production_panic_patterns_are_absent() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();

    for path in rust_source_files(&root.join("src")) {
        if rust_source_path_is_test_only(&path, &root) {
            continue;
        }

        let content = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        violations.extend(production_panic_pattern_violations(
            &relative_path_for_display(&root, &path),
            &content,
        ));
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

fn rust_source_path_is_test_only(path: &Path, root: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    if relative
        .components()
        .any(|component| component.as_os_str() == "tests")
    {
        return true;
    }

    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with("_tests.rs") || name.ends_with("_test.rs"))
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
            if let Some((is_test_only, next)) = macro_test_only_attr_at(&tokens, idx) {
                idx = if is_test_only {
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

fn macro_test_only_attr_at(tokens: &[TokenTree], start: usize) -> Option<(bool, usize)> {
    if !matches!(tokens.get(start), Some(TokenTree::Punct(punct)) if punct.as_char() == '#') {
        return None;
    }

    let Some(TokenTree::Group(group)) = tokens.get(start + 1) else {
        return None;
    };
    if group.delimiter() != Delimiter::Bracket {
        return None;
    }

    let is_test_only = syn::parse2::<Meta>(group.stream())
        .ok()
        .is_some_and(|meta| meta_is_test_only_attr(&meta));
    Some((is_test_only, start + 2))
}

fn macro_attr_target_end(tokens: &[TokenTree], start: usize) -> usize {
    if start >= tokens.len() {
        return start;
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
