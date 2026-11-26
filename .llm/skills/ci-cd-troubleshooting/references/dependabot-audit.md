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

List the tracked lockfiles with `git ls-files -- Cargo.lock ':(glob)**/Cargo.lock'`.
This includes both Fortress clients. Check which lockfiles contain the affected
crate and version.

## Fix

1. For each affected lockfile, run
   `cargo update --manifest-path path/to/Cargo.toml -p crate@affected --precise fixed`.
   Use the manifest next to the lockfile and substitute the crate and versions.
2. Keep `versioning-strategy: lockfile-only` and `allow: [{dependency-type: all}]`
   on exact-release fixtures. This lets Dependabot update transitive lock
   entries and keeps the manifests pinned.
3. Replay saved encoding samples against their recorded Rust wire output. Run
   the live encoding check to verify wire agreement with current dependencies.

## Verify

```bash
(
  set -eo pipefail
  git ls-files -- Cargo.lock ':(glob)**/Cargo.lock' |
    while IFS= read -r lockfile; do
      cargo audit --file "$lockfile"
    done
  cargo deny --manifest-path Cargo.toml --all-features check
)
```

The audit block stops on the first failed check. Run the live encoding check
with `python3 benches/encoding/cross_language.py --check` and replay a saved
cohort with `python3 benches/encoding/cross_language.py --verify-recorded <cohort>`.

## Related

- [CI CD Troubleshooting Ecosystem](ecosystem-and-toolchains.md)
- [Supply Chain Audit Policy](../../supply-chain-security/SKILL.md)
