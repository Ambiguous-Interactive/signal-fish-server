# CI tool bootstrap

Pinned GitHub release assets in CI and the devcontainer use
`scripts/download-verified.sh`. The helper keeps the browser download route
first, retries transient failures, reuses an existing verified file, and falls
back to the official GitHub API asset endpoint.

## Download contract

- Each call pins an exact URL and digest. SHA-256 is the default; SHA-512 is
  available for the Godot archives.
- An existing output file is reused only when its bytes match the pin. A
  changed or truncated cache entry is removed before the download.
- Downloads go to a temporary file. A checksum failure or exhausted retry
  removes that file, so unverified bytes never remain at the output path.
- The API fallback accepts only one exact asset name with a positive integer
  ID from the pinned release tag. The original digest still verifies the
  asset bytes.
- `sha256sum` and `sha512sum` are preferred; `shasum -a 256` and
  `shasum -a 512` cover macOS and other hosts without the GNU tools.
- `DOWNLOAD_VERIFIED_MAX_TIME` bounds each attempt. The default is 60
  seconds; the large Godot archives use 1200 seconds. Zero and leading-zero
  values are rejected before curl starts.
- Every GitHub release download in `.github/workflows`, `scripts/`, and
  `.devcontainer/Dockerfile` must name this helper. A repository test enforces
  that rule.

`scripts/install-hax-tools.sh` wraps `cargo hax tools install` with three
bounded attempts and a delay. hax owns the Charon and Aeneas transport and
verifies its embedded checksums. The wrapper cannot add the API fallback
inside hax, but it makes a transient failure visible and recoverable. The
`~/.cache/hax` cache is restored before the install step in every Lean job.

## Measurements

Environment: Linux ARM64, 12 vCPU, 31 GiB memory, 2026-11-27. Each
`download-verified.sh` row is three cold runs and three warm runs. Times are
medians. A cold run starts with no output file. A warm run reuses the verified
file and does not touch the network.

| Asset | Bytes | Cold | Warm |
| --- | ---: | ---: | ---: |
| elan 4.2.4 x86_64 archive | 4,996,571 | 0.500 s | 0.024 s |
| lychee 0.24.2 aarch64 MUSL | 6,962,068 | 0.613 s | 0.028 s |
| yq v4.54.1 arm64 | 13,172,896 | 0.930 s | 0.044 s |
| actionlint 1.7.12 arm64 | 2,111,482 | 0.339 s | 0.014 s |
| hax Charon and Aeneas cache | 475 MiB | 10.102 s | 0.150 s |

The elan archive is the exact x86_64 asset and digest used by CI. It was
downloaded on ARM only for this measurement; the binary was not executed. The
hax row uses the wrapper with an isolated `XDG_CACHE_HOME`, three attempts,
and no retry delay. A direct `cargo hax tools install` measured 10.445 s cold
and 0.077 s warm.

Cold setup is dominated by network transfer. Warm setup is dominated by
digest verification. The hax cache is the largest cold cost because it holds
the Charon and Aeneas binaries and the pinned nightly Rust toolchain.

## External assumptions and limits

- GitHub must serve the browser route or the API asset endpoint. A broad
  GitHub outage can exhaust both. The job then fails closed; a later CI run
  retries the same pins.
- The API fallback is unauthenticated. It makes at most one metadata request
  and one asset request per failed primary download, which stays far below the
  public hourly rate limit. Rate-limit responses fail closed.
- `hax` controls its own download and extraction logic. A future hax release
  can change retry behavior, cache layout, or asset names. The wrapper keeps
  the pinned versions in `formal/source-linked/lean/hax.toml` unchanged.
- A clean Docker build cannot reuse a verified cache from an earlier build.
  The helper still supplies the API fallback and the digest check. Docker
  layer caching reuses the completed install layer when its inputs do not
  change.
- The large Godot archive timeout is bounded by
  `DOWNLOAD_VERIFIED_MAX_TIME`, not by an open-ended transfer.

## Reproduction

```bash
# Cold download, then a network-free warm reuse with the same pin.
bash scripts/download-verified.sh \
  https://github.com/leanprover/elan/releases/download/v4.2.4/elan-x86_64-unknown-linux-gnu.tar.gz \
  42b94d4244e8353142c456ec0e4ca6528fd898a6c604d4059f494e706e431f63 \
  /tmp/elan.tar.gz
bash scripts/download-verified.sh \
  https://github.com/leanprover/elan/releases/download/v4.2.4/elan-x86_64-unknown-linux-gnu.tar.gz \
  42b94d4244e8353142c456ec0e4ca6528fd898a6c604d4059f494e706e431f63 \
  /tmp/elan.tar.gz

# Bounded hax tool install with an isolated verified cache.
XDG_CACHE_HOME="$(mktemp -d)" bash scripts/install-hax-tools.sh
```

The focused controls are:

```bash
cargo nextest run --test ci_config_tests \
  -E 'test(/verified_downloader|hax_tool_install|ci_release_downloads/)'
```
