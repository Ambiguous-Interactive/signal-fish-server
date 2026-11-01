# Devcontainer storage

Keep daily builds warm. Remove disposable experiment output after use.
Shared Docker and editor storage requires explicit host maintenance.

## Measure first

Run these commands inside the devcontainer:

```bash
python3 scripts/dev-storage.py report
python3 scripts/dev-storage.py report --json --docker > /path/outside-scratch/storage.json
```

The report records allocated bytes, available filesystem bytes, mounts, target
subdirectories, Docker references, builder policy, and collection errors.
Docker queries have time limits. A timeout means unknown usage, not zero usage.
Malformed Docker records remain attached to the affected query as collection
errors. They do not discard the filesystem measurements or valid queries.
For a large Docker store, retry with `report --json --docker --docker-timeout 180`.
The detailed reference inventory uses batches of 64 containers.
Do not sum shared filesystem totals, overlapping directories, or image sizes.
A writable-layer total includes home-directory and `/tmp` usage.

The target warning is 40 GiB. The maintenance threshold is 60 GiB.
These are operational thresholds, not filesystem quotas. `report` prints their
state; it does not delete builds or change its exit status for a budget warning.
Check it after large experiments and during weekly maintenance.

## Run disposable experiments

```bash
python3 scripts/dev-storage.py run-scratch -- cargo check --locked --all-features
python3 scripts/dev-storage.py run-scratch -- bash scripts/check-no-panics.sh
python3 scripts/dev-storage.py clean --dry-run
python3 scripts/dev-storage.py clean --apply
```

The wrapper runs from the repository root. It sets `CARGO_TARGET_DIR`,
`NESTED_CARGO_TARGET_DIR`, `TMPDIR`, and `SIGNAL_FISH_SCRATCH_DIR` inside
`target/managed-scratch/run-<id>/work`. Commands that explicitly override these
paths remain responsible for their own output. Use the wrapper for temporary
sanitizer, Miri, tool-download, and isolated-build experiments. Use the normal
target directory for daily builds and scoped tests.

The wrapper removes successful work. Copy evidence that must survive to a
durable report before the command exits. Keep raw local evidence in ignored
`progress/`. Failed runs remain for seven days. Completed runs also share a
20 GiB retention budget. Cleanup removes the oldest eligible runs first.
`clean --budget-gib <number>` changes the budget for that invocation.
An unlocked interrupted run becomes eligible after seven days.
Active leases can exceed the retention budget while work runs.
Post-command metadata or cleanup failures warn and preserve the command's exit
status. They do not turn a successful command into a failure.
Failed metadata writes remove their own temporary file and preserve the prior
saved record. Each write uses a distinct temporary name.

Cleanup requires matching project and run ownership markers. It preserves
unknown entries, active leases, Git data, and nested mounts. It never follows
symlinks to delete their targets. It removes only managed scratch; it does not
clean legacy target trees, npm caches, agent history, or shared editor data.
Removal opens each directory component without following links, validates the
leased run through its open handle, and removes files relative to that handle.
A replaced run entry cannot redirect deletion to another directory.

Post-start runs cleanup with an eight-second timeout, before the agent-refresh
opt-out. It warns and continues on failure. Set
`SIGNAL_FISH_SKIP_STORAGE_CLEANUP=1` to skip it independently. Experiment
completion also runs cleanup. After a timeout, ownership metadata remains so
the next cleanup can retry. Interrupted metadata writes or unknown content
require manual inspection.

To reset the normal development cache, stop Rust Analyzer and all local builds,
preserve needed reports, then run `cargo clean --profile dev`. Rebuild only the
needed test target. Do not schedule this reset on every launch. Keep separate
instrumented builds where their flags require isolation.

## Host maintenance

Run the host collector in PowerShell 7 on Windows, outside the container:

```powershell
pwsh -File scripts/dev-storage-host.ps1 > storage-host-before.json
```

Use `-DockerWslPath <directory>` if Docker Desktop moved its disk. The collector
records drive free bytes, VHD file length, allocated VHD bytes, and WSL layout.
It captures raw WSL output and decodes UTF-16LE or UTF-8 before recording names.
Microsoft documents the UTF-16LE list-output behavior in its
[WSL output notes](https://github.com/microsoft/WindowsDeveloperConfig/blob/main/wsl-comfort/readme.md).
An empty VHD list means the disk was not found. Keep these reports local.

1. Inspect the Docker reference snapshot. Preserve running containers and
   other projects. Review files in stopped containers before removing them.
2. Invoke `docker rm <reviewed-container-ids>` only for disposable containers.
   Recheck all container mounts before `docker volume rm <reviewed-volume-names>`.
   A stopped container still references its volumes. An anonymous name does
   not prove that data is disposable.
3. Remove reviewed unused images by exact ID. Do not add image sizes together;
   images share layers. Mark future project experiment containers with
   `--label dev.signal-fish.disposable=true --rm`. Labels identify ownership;
   they do not prove old unlabelled containers are safe to delete.
4. Inspect `docker buildx inspect default`. For the default Docker driver,
   merge this policy into Docker Desktop **Settings > Docker Engine** if it
   is absent. Preserve unrelated settings. Applying it restarts Docker.

   ```json
   { "builder": { "gc": { "enabled": true, "defaultKeepStorage": "20GB" } } }
   ```

5. Review `/vscode/vscode-server/bin` and `extensionsCache` separately. Before
   removing an old server directory, check every running container's editor
   processes. Keep active versions and one fallback. Leave installed extensions
   and editor data intact. Run this maintenance explicitly while editors are
   stopped; do not automate deletion from the shared volume on project launch.
6. Record Linux free space. Quit Docker Desktop during a maintenance window,
   then rerun the Windows collector and compare allocated bytes and drive free
   space. Docker documents automatic compaction for supported WSL layouts.
   A Linux free-space gain alone does not prove Windows reclaimed bytes.

BuildKit GC covers build cache, not volumes or container writable layers.
See [Docker GC](https://docs.docker.com/build/cache/garbage-collection/).
Cargo automatic cleanup covers downloaded dependency caches, not `target`
build output. See [Cargo cache policy](https://doc.rust-lang.org/cargo/reference/config.html#cache).
Docker introduced automatic host reclamation for supported WSL layouts in
4.34. See [Docker disk reclamation](https://www.docker.com/blog/docker-desktop-4-34/).
If the measured disk does not shrink, identify its exact layout and follow the
applicable offline compaction procedure. Do not compact a running VHD, assume a
`docker-desktop-data` distribution exists, or zero-fill a nearly full disk.

## Evidence from 2026-11-01

The observed environment was Windows ARM64, WSL2, and Docker Desktop 4.97.0.
The project source was a Windows bind mount. Rust output was a named volume.

| Observation | Measured value |
| --- | ---: |
| Docker Linux filesystem used | About 955 GiB |
| Project target before recovery | About 119 GiB |
| Shared VS Code volume | About 39 GiB |
| Container writable layer, including home and `/tmp` | About 31 GiB |
| `/tmp`, included above | About 18 GiB |
| Stopped containers across the host | 1,324 |
| Volumes across the host | 1,491 |
| Anonymous volumes | 1,414 |
| Volumes referenced by all inspected containers | 1,354 |
| Volumes unreferenced in that snapshot | 137 |

Reduced Rust debug information was already configured. The main confirmed
project cause was retained build variants without a retention policy. Ten
reviewed build trees had no visible process references, Git data, or nested
mounts. Their newest files were 10–94 days old. The one-time recovery removed
these exact disposable variants: `asan-local`, `tsan-local`, `asan-pin`,
`no-panic-policy-scan`, `session-436-package-cold-baseline`, `session507-asan`,
`miri-opt-test`, `native-no-panic-audit`, `miri`, and `aarch64-unknown-linux-gnu`.
Normal `debug` and `release` caches remained. Source and Git worktrees remained.

The removed trees accounted for 71,692,668,928 allocated bytes. Available Linux
space rose from 1,250,828,288 to 72,943,468,544 bytes: a measured gain of
71,692,640,256 bytes, or **66.77 GiB**. This is a Linux filesystem result.
Windows VHD allocation was not accessible from this container.

The first post-recovery report measured 51.58 GiB in the project target, below
the 60 GiB maintenance threshold. A real-volume probe retained an 8 MiB failed
run, listed it in dry-run mode, removed it with an explicit zero-budget cleanup,
and left no completed runs after a successful command.

The inspected default builder already had its 20 GiB GC policy. No builder
setting was changed. No shared containers, images, volumes, or editor versions
were removed. The initial 20-second Docker accounting query timed out. A later
query with a 180-second limit completed:

| Host-wide Docker category | Reported size | Docker-reported reclaimable size |
| --- | ---: | ---: |
| Images | 166.5 GB | 87.81 GB |
| Container writable layers | 301.5 GB | 138.8 GB |
| Local volumes | 340 GB | 127.6 GB |
| Build cache | 109.3 GB | 20.8 GB |

These are Docker's decimal GB figures from a later snapshot. Images and build
cache can share storage; do not sum these rows as independent physical usage.
The figures establish substantial host-wide accumulation beyond this project.
Reclaimable means unused according to Docker, not disposable according to the
owner. Existing GC policy does not bound all shared image data, writable layers,
or volumes. Keep shared-object removal as explicit, reviewed maintenance.

## Validation and follow-up

```bash
python3 scripts/test_dev_storage.py
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/test-dev-storage-host.ps1
shellcheck --severity=warning .devcontainer/post-start.sh
bash scripts/check-tooling-parity.sh
cargo nextest run --test agent_tooling_guards -E 'test(scratch_cleanup_is_bounded_and_independent_of_agent_refresh)'
```

The Python suite uses disposable fixtures. It checks dry-run immutability,
expiry, oldest-first eviction, leases, signal interruption, command failure,
symlinks, hardlinks, foreign ownership, Git preservation, and lifecycle failure.
The devcontainer build workflow runs it before the real post-start path.

Over seven representative workdays, collect a daily storage report. Pass when
completed managed scratch stays within 20 GiB after cleanup, total target use
stays below 60 GiB, and exceptions have an owner. Measure scoped test and Clippy
commands over five warm runs before and after changes; target a median slowdown
of no more than 10%. Host reclamation, hosted image validation, and the seven-day
observation require separate results. Do not mark them complete from local
fixture tests.

Local validation passed 34 Python tests, all 14 agent-tooling guards, the full
Clippy gate with all targets and features, formatting, tooling parity, document
consistency, shellcheck, actionlint, and PowerShell syntax parsing. Removing the
lifecycle cleanup invocation made the new guard fail; restoring it made it pass.
PowerShell fixtures also verify WSL byte decoding and raw stdout/stderr capture.
The Windows allocation API still needs a real host measurement. CI runs both
storage suites with strict shell error handling, so an earlier failure cannot
be hidden by a later successful command. Initial local validation did not run
the hosted image workflow. Publication checks are tracked in the
[storage retention PR](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/1346).

Five warm samples per command gave these medians:

| Command | Before recovery | After recovery, first cohort | Repeat cohort |
| --- | ---: | ---: | ---: |
| Scoped agent-tooling nextest check | 0.522 s | 0.622 s | 0.774 s |
| Scoped agent-tooling Clippy | 0.341 s | 0.335 s | 0.324 s |

The nextest command exceeded the 10% timing target in both cohorts. The first
cohort overlapped the full Clippy gate; the repeat ran after it finished.
The cause of the nextest timing increase is unresolved. No Cargo profile or
compiler flag changed. This result does not establish a performance regression
in the cleanup tool, which is not on either measured command path, but it also
does not pass the planned timing gate. Repeat under a controlled host workload
before claiming that gate is met. Keep the storage recovery result separate
from that open timing result.
