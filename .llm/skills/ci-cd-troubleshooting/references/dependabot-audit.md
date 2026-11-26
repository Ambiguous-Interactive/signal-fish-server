# Every Dependabot Pull Request Red on `Dependency Audit`

## Symptom

All Dependabot pull requests fail `Dependency Audit`, `Enable auto-merge for
Dependabot PRs`, and `Required PR Result`. `main` push CI fails the same way.
Every other job passes.

## Root Cause

`Dependency Audit` runs `cargo audit --file` against every tracked lockfile.
The exact-release fixture graphs (`third_party/webrtc`, `third_party/rtc`,
`third_party/rtc-sctp`, `clients/fortress`, `clients/fortress-wasm`) had no
Dependabot coverage, so a new transitive advisory stayed in those locks
forever. One stale lock fails the job for every pull request.

The 2026-11 trigger was `url` 2.5.8 (RUSTSEC-2026-0592). It was present in the
root, native, fuzz, and all three `third_party` locks.

The encoding lane has a second trap. `benches/encoding/Cargo.lock` and
`cross_language.py` are hashed into the recorded `environment.json`. Any
dependency bump there used to fail `--verify-recorded` replay.

## Diagnosis

```bash
# Which graphs still hold the vulnerable version?
for f in $(git ls-files | grep 'Cargo.lock$'); do
  echo "== $f"; grep -A1 '^name = "<crate>"$' "$f"
done
cargo audit --file Cargo.lock
```

## Fix

1. Update the advisory in every audited lockfile. Run
   `cargo update -p <crate> --precise <fixed>` in `/`, `clients/native`,
   `fuzz`, `third_party/webrtc`, `third_party/rtc`, and `third_party/rtc-sctp`.
2. Keep the fixture lockfiles on Dependabot with
   `versioning-strategy: lockfile-only`. It refreshes transitive locks and
   never edits the pinned fixture manifest.
3. Keep `Cargo.lock` and `cross_language.py` as provenance during cohort
   replay. Wire agreement still runs against the current graph.

## Verify

```bash
for f in Cargo.lock clients/native/Cargo.lock fuzz/Cargo.lock \
  third_party/webrtc/Cargo.lock third_party/rtc/Cargo.lock \
  third_party/rtc-sctp/Cargo.lock; do
  cargo audit --file "$f"
done
cargo deny --manifest-path Cargo.toml --all-features check
```

Tests: `test_dependabot_cargo_directories_cover_every_automated_package`, and
`python3 benches/encoding/cross_language.py --verify-recorded <cohort>`.

## Related

- [CI CD Troubleshooting Ecosystem](ecosystem-and-toolchains.md)
- [Supply Chain Audit Policy](../../supply-chain-security/SKILL.md)
