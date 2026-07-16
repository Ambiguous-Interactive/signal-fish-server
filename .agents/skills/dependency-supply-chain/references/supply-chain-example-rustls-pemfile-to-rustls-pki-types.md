# Supply Chain Example - rustls-pemfile to rustls-pki-types Migration

**Applies to**: When `cargo deny` reports unmaintained advisory RUSTSEC-2025-0134 for `rustls-pemfile`.

---

## Incident

**Problem:** `rustls-pemfile` was flagged unmaintained; PEM parsing moved to `rustls-pki-types`.

---

## Symptoms

- Advisory findings in dependency policy checks
- Ongoing risk of unpatched vulnerabilities due to unmaintained crate status

---

## Migration

```toml
# BEFORE: tls = ["axum-server", "rustls", "rustls-pemfile"]
# AFTER:
tls = ["axum-server", "rustls", "rustls-pki-types"]
```

```rust
// BEFORE: use rustls_pemfile::certs;
// AFTER:
use rustls_pki_types::{pem::PemObject, CertificateDer};
let certs: Vec<CertificateDer> = CertificateDer::pem_file_iter(path)
    .collect::<Result<Vec<_>, _>>()?;
```

---

## Related References

- [Supply Chain Audit Policy](./supply-chain-audit-policy.md) — Audit and remediation policy
- [Dependency Management Cargo](./dependency-management-cargo.md) — Dependency change process
- [CI CD Troubleshooting Supply Chain](../../ci-troubleshooting/references/ci-cd-troubleshooting-supply-chain.md) —
  Supply-chain CI troubleshooting patterns
