//! Config-validation coverage analyzer (#213).
//!
//! The config seam has shipped a recurring defect class: a numeric knob that
//! is never consulted by any startup-validation guard, so a legal config
//! value silently defers a total-rejection or data-loss failure to request
//! time — zero total-rejection caps (#430/#431), the deferred
//! `default_max_players` mispairing, and the zero `inactive_room_timeout`
//! occupied-room GC were all exactly this shape (session-197 sweep).
//!
//! This scan closes the class structurally: **every numeric field of every
//! config struct under `src/config/` must either be referenced by a startup
//! validation guard (`validate_config_security` in `validation.rs`, or a
//! `validate*` function in a config module) or appear, with a recorded
//! rationale, in [`EXEMPT_NUMERIC_FIELDS`]**. Adding a numeric config field
//! without deciding its validation story fails the scan.
//!
//! Soundness scope (deliberate over-approximations, kept for simplicity):
//! - "Referenced by a guard" is approximated by identifier collection: ALL
//!   identifiers in `validation.rs` (its `mod tests` is skipped) and all
//!   identifiers in every `fn validate*` elsewhere under `src/config/`. A
//!   field whose name appears in `validation.rs` for an unrelated reason is
//!   counted as covered; the exemption list is the mechanism for fields with
//!   no true guard, and the Z3 config-closure proof set (proof set J) checks
//!   the admitted set's actual semantics.
//! - Guard detection is name-based (`fn validate*`). Every such function in
//!   `src/config/` is a real validation guard today; a future non-guard
//!   `validate*` function that merely mentions a field would mask it here.

use std::fs;
use std::path::{Path, PathBuf};

use syn::visit::Visit;

/// Numeric fields that are deliberately NOT covered by a startup validation
/// guard, with the reason each is safe to leave unguarded. Every entry must
/// name a real numeric config field (stale entries fail the scan).
const EXEMPT_NUMERIC_FIELDS: &[(&str, &str)] = &[
    (
        "port",
        "0 is the OS ephemeral-port assignment; binding :0 is a standard \
         dynamic-deployment mode, not a misconfiguration",
    ),
    (
        "drain_grace_secs",
        "0 is the documented legacy-disable knob: immediate shutdown without \
         the drain grace (ServerConfig field doc)",
    ),
    (
        "empty_room_timeout",
        "rooms are always created occupied (creation seats the creator), so 0 \
         only shortens the post-vacation linger window; reconnection-protected \
         rooms are shielded by the GC's protection set",
    ),
    (
        "max_signal_errors",
        "0 legitimately suppresses every detailed rejection from the first \
         failure (anti-enumeration posture); the valid-signal budget is \
         independent and separately validated > 0",
    ),
    (
        "membership_snapshot_interval_secs",
        "reserved for a future membership-snapshot backend and currently \
         unconsumed (CoordinationConfig field doc)",
    ),
    (
        "dashboard_cache_refresh_interval_secs",
        "clamped to a >=1s floor at the consumption site (server.rs `.max(1)`)",
    ),
    (
        "dashboard_cache_ttl_secs",
        "clamped to a >=1s floor at the consumption site (server.rs `.max(1)`)",
    ),
    (
        "dashboard_cache_history_window_secs",
        "clamped to a >=1s floor at the consumption site (server.rs `.max(1)`)",
    ),
];

#[test]
fn test_every_numeric_config_field_is_validated_or_exempt_with_rationale() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let config_dir = root.join("src").join("config");
    assert!(
        config_dir.is_dir(),
        "scan root {} is missing — update the scan if the config module moved",
        config_dir.display()
    );

    let files: Vec<(String, String)> = rust_source_files(&config_dir)
        .into_iter()
        .map(|path| {
            let display = relative_path_for_display(&root, &path);
            let content = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {display}: {error}"));
            (display, content)
        })
        .collect();

    let validated = validated_identifiers(&files);
    let mut violations = Vec::new();
    for (file, content) in &files {
        for (struct_name, field_name) in numeric_config_fields(file, content) {
            if validated.contains(field_name.as_str()) {
                continue;
            }
            if EXEMPT_NUMERIC_FIELDS
                .iter()
                .any(|(name, _)| *name == field_name)
            {
                continue;
            }
            violations.push(format!(
                "{file}: `{struct_name}.{field_name}` is numeric and is referenced by no \
                 startup validation guard — either validate it in \
                 `validate_config_security`/its delegated `validate()` (reject or bound the \
                 dangerous values) or add it to EXEMPT_NUMERIC_FIELDS with a recorded rationale"
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "Every numeric config field must have a decided validation story \
         (startup guard or documented exemption):\n{}",
        violations.join("\n")
    );
}

#[test]
fn test_exempt_entries_name_real_numeric_config_fields() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let config_dir = root.join("src").join("config");
    let files: Vec<(String, String)> = rust_source_files(&config_dir)
        .into_iter()
        .map(|path| {
            let content = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            (relative_path_for_display(&root, &path), content)
        })
        .collect();

    let all_fields: Vec<String> = files
        .iter()
        .flat_map(|(file, content)| {
            numeric_config_fields(file, content)
                .into_iter()
                .map(|(_, field)| field)
        })
        .collect();

    let stale: Vec<&str> = EXEMPT_NUMERIC_FIELDS
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !all_fields.iter().any(|field| field == *name))
        .collect();

    assert!(
        stale.is_empty(),
        "EXEMPT_NUMERIC_FIELDS lists names that are no longer numeric config fields \
         (renamed or removed?): {stale:?} — drop or update the stale entries"
    );
}

#[test]
fn test_detector_flags_unvalidated_numeric_fields_only() {
    let validated_module = (
        "config_good.rs".to_string(),
        r#"
            pub struct AlphaConfig { pub retries: u32 }
            impl AlphaConfig {
                fn validate(&self) { let _ = self.retries; }
            }
        "#
        .to_string(),
    );
    let unvalidated_module = (
        "config_bad.rs".to_string(),
        "pub struct BetaConfig { pub window: u64 }".to_string(),
    );

    let files = vec![validated_module, unvalidated_module];
    let validated = validated_identifiers(&files);
    let mut violations = Vec::new();
    for (file, content) in &files {
        for (struct_name, field_name) in numeric_config_fields(file, content) {
            if !validated.contains(field_name.as_str()) {
                violations.push(format!("{file}: {struct_name}.{field_name}"));
            }
        }
    }

    assert_eq!(
        violations,
        vec!["config_bad.rs: BetaConfig.window".to_string()],
        "the detector must flag exactly the field with no validate* reference"
    );
}

// ---------------------------------------------------------------------------
// Mechanics
// ---------------------------------------------------------------------------

const NUMERIC_TYPES: &[&str] = &[
    "u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize",
];

/// `(struct name, field name)` pairs for every numeric field of every struct
/// in a config module file.
fn numeric_config_fields(file: &str, content: &str) -> Vec<(String, String)> {
    let parsed = syn::parse_file(content)
        .unwrap_or_else(|error| panic!("failed to parse {file} as Rust syntax: {error}"));
    let mut visitor = NumericFieldVisitor::default();
    visitor.visit_file(&parsed);
    visitor.fields
}

#[derive(Default)]
struct NumericFieldVisitor {
    current_struct: Option<String>,
    fields: Vec<(String, String)>,
}

impl<'ast> Visit<'ast> for NumericFieldVisitor {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        // Test-helper structs must not join the coverage contract.
        if item.ident == "tests" {
            return;
        }
        self.current_struct = None;
        syn::visit::visit_item_mod(self, item);
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.current_struct = Some(item.ident.to_string());
        syn::visit::visit_item_struct(self, item);
        self.current_struct = None;
    }

    fn visit_field(&mut self, field: &'ast syn::Field) {
        if let (Some(struct_name), Some(type_name)) =
            (&self.current_struct, field_type_ident(field))
        {
            if NUMERIC_TYPES.contains(&type_name.as_str()) {
                if let Some(field_name) = field.ident.as_ref() {
                    self.fields
                        .push((struct_name.clone(), field_name.to_string()));
                }
            }
        }
        syn::visit::visit_field(self, field);
    }
}

fn field_type_ident(field: &syn::Field) -> Option<String> {
    let syn::Type::Path(path) = &field.ty else {
        return None;
    };
    // Only bare primitive paths (no `std::…`, no generics).
    if path.qself.is_some() || path.path.segments.len() != 1 {
        return None;
    }
    Some(path.path.segments.last()?.ident.to_string())
}

/// Every identifier referenced by `validation.rs` (the whole guard set)
/// plus the bodies of every `fn validate*` in the config modules (the
/// delegated per-struct guards).
fn validated_identifiers(files: &[(String, String)]) -> std::collections::BTreeSet<String> {
    let mut identifiers = std::collections::BTreeSet::new();
    for (file, content) in files {
        let parsed = syn::parse_file(content)
            .unwrap_or_else(|error| panic!("failed to parse {file} as Rust syntax: {error}"));

        if file.ends_with("validation.rs") {
            // Whole-file identifier collection, minus the test module:
            // test-only references must not count as guard coverage.
            let mut collector = IdentCollector::default();
            for item in &parsed.items {
                if let syn::Item::Mod(item_mod) = item {
                    if item_mod.ident == "tests" {
                        continue;
                    }
                }
                collector.visit_item(item);
            }
            identifiers.extend(collector.identifiers);
            continue;
        }

        for item in &parsed.items {
            match item {
                syn::Item::Fn(function) if is_guard_fn(&function.sig) => {
                    let mut collector = IdentCollector::default();
                    collector.visit_item_fn(function);
                    identifiers.extend(collector.identifiers);
                }
                syn::Item::Impl(item_impl) => {
                    for impl_item in &item_impl.items {
                        if let syn::ImplItem::Fn(method) = impl_item {
                            if is_guard_fn(&method.sig) {
                                let mut collector = IdentCollector::default();
                                collector.visit_impl_item_fn(method);
                                identifiers.extend(collector.identifiers);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    identifiers
}

fn is_guard_fn(sig: &syn::Signature) -> bool {
    sig.ident.to_string().starts_with("validate")
}

#[derive(Default)]
struct IdentCollector {
    identifiers: Vec<String>,
}

impl<'ast> Visit<'ast> for IdentCollector {
    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        self.identifiers.push(ident.to_string());
    }
}

fn rust_source_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rust_source_files(dir, &mut files);
    files.sort();
    files
}

fn collect_rust_source_files(dir: &Path, files: &mut Vec<PathBuf>) {
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
