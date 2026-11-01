#!/usr/bin/env python3
"""Measure development storage and retain only bounded, owned scratch builds.

No third-party packages. Cleanup is Linux-only and never touches unmarked data.
"""

import argparse
from contextlib import contextmanager
import json
import math
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import time
import uuid

OWNER = "signal-fish-dev-storage-v1"
GIB = 1024**3
DEFAULT_BUDGET = 20 * GIB
RETENTION = 7 * 86400


def write_json(path, data):
    """Replace metadata atomically; a failed write cannot create a valid run."""
    temporary = path.with_suffix(f".{uuid.uuid4().hex}.tmp")
    created = False
    try:
        with temporary.open("x", encoding="utf-8") as stream:
            created = True
            json.dump(data, stream, sort_keys=True)
        temporary.replace(path)
    finally:
        if created:
            try:
                temporary.unlink(missing_ok=True)
            except OSError as error:
                print(f"[storage] Could not remove metadata temporary file: {error}", file=sys.stderr)


@contextmanager
def lock_file(path):
    """Use non-blocking leases. Children inherit the run lease on exec."""
    import fcntl

    descriptor = os.open(path, os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise ValueError(f"Lease is not a regular file: {path}")
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield descriptor
    finally:
        os.close(descriptor)


def allocated_bytes(path):
    """Count allocated blocks once per inode; never follow directory links."""
    seen = set()
    total = 0
    for parent, directories, files in os.walk(path, followlinks=False):
        for entry in [Path(parent), *(Path(parent) / x for x in files),
                      *(Path(parent) / x for x in directories
                        if (Path(parent) / x).is_symlink())]:
            info = entry.lstat()
            identity = (info.st_dev, info.st_ino)
            if identity not in seen:
                total += info.st_blocks * 512
                seen.add(identity)
    return total


def regular_json(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, encoding="utf-8") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > 16384:
            raise ValueError(f"Metadata is not a regular file: {path}")
        return json.load(stream)


def remove_run(path, lease_fd, repository, validate):
    """Keep ownership and lease metadata until the large tree is gone.

    A lifecycle timeout during removal leaves a recognizable run to retry.
    """
    if not shutil.rmtree.avoids_symlink_attacks:
        raise ValueError("This platform cannot safely remove scratch trees")
    # Open each component without following links. Keep the validated run
    # anchored even if its directory entry changes during removal.
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    descriptors = []
    try:
        descriptors.append(os.open(repository, flags))
        parts = path.relative_to(repository).parts
        for component in parts[:-1]:
            descriptors.append(os.open(component, flags, dir_fd=descriptors[-1]))
        parent_fd = descriptors[-1]
        run_fd = os.open(parts[-1], flags, dir_fd=parent_fd)
        descriptors.append(run_fd)
        identity = os.fstat(run_fd)
        actual_lease = os.stat("lease", dir_fd=run_fd, follow_symlinks=False)
        if not os.path.samestat(os.fstat(lease_fd), actual_lease):
            raise ValueError("Run lease changed during removal")
        validate(path, directory_fd=run_fd)

        def verify_entry():
            current = os.stat(parts[-1], dir_fd=parent_fd, follow_symlinks=False)
            if not os.path.samestat(identity, current):
                raise ValueError("Run directory changed during removal")

        verify_entry()
        try:
            work = os.stat("work", dir_fd=run_fd, follow_symlinks=False)
        except FileNotFoundError:
            work = None
        if work is not None:
            if stat.S_ISLNK(work.st_mode):
                os.unlink("work", dir_fd=run_fd)
            else:
                shutil.rmtree("work", dir_fd=run_fd)
        verify_entry()
        os.unlink("run.json", dir_fd=run_fd)
        os.unlink("lease", dir_fd=run_fd)
        verify_entry()
        os.rmdir(parts[-1], dir_fd=parent_fd)
    finally:
        for descriptor in reversed(descriptors):
            os.close(descriptor)


def mounted_below(path):
    """Detect bind mounts too; os.path.ismount alone misses same-device binds."""
    for line in Path("/proc/self/mountinfo").read_text(encoding="utf-8").splitlines():
        mount = Path(re.sub(r"\\([0-7]{3})", lambda match: chr(int(match[1], 8)),
                            line.split()[4]))
        if mount == path or path in mount.parents:
            return True
    return False


class Manager:
    def __init__(self, repository):
        self.repository = Path(repository).resolve()
        self.base = self.repository / "target" / "managed-scratch"

    def validate_path(self):
        for path in [self.repository / "target", self.base]:
            if path.is_symlink():
                raise ValueError(f"Managed path must not be a symlink: {path}")

    def initialize(self):
        self.validate_path()
        self.base.mkdir(parents=True, exist_ok=True)
        marker = self.base / ".owner.json"
        if not marker.exists():
            # Never adopt pre-existing, unmarked content.
            if list(self.base.iterdir()):
                raise ValueError(f"Unmarked scratch directory: {self.base}")
            write_json(marker, {"owner": OWNER, "repository": str(self.repository)})
            (self.base / ".manager.lock").touch(exist_ok=False)
        self.validate()

    def validate(self):
        self.validate_path()
        marker = regular_json(self.base / ".owner.json")
        if marker != {"owner": OWNER, "repository": str(self.repository)}:
            raise ValueError(f"Scratch ownership mismatch: {self.base}")

    def inspect_run(self, path, directory_fd=None):
        if directory_fd is None and (path.is_symlink() or not path.is_dir()):
            raise ValueError("Not a managed directory")
        root = path if directory_fd is None else Path(f"/proc/self/fd/{directory_fd}")
        metadata = regular_json(root / "run.json")
        if (not isinstance(metadata, dict) or metadata.get("owner") != OWNER
                or metadata.get("repository") != str(self.repository)
                or metadata.get("state") not in {"running", "failed", "complete"}):
            raise ValueError("Run ownership or state mismatch")
        if {entry.name for entry in root.iterdir()} - {"work", "run.json", "lease"}:
            raise ValueError("Contains unclassified files")
        actual_path = path if directory_fd is None else Path(os.readlink(root))
        if mounted_below(actual_path):
            raise ValueError("Contains a mount")
        timestamp = metadata.get("completed")
        if timestamp is None:
            timestamp = metadata.get("created")
        if (not isinstance(timestamp, (int, float)) or not math.isfinite(timestamp)
                or timestamp <= 0 or timestamp > time.time() + 60):
            raise ValueError("Invalid run timestamp")
        # Refuse source trees and nested mounts, even if someone moved one here.
        for parent, directories, files in os.walk(root, followlinks=False):
            if ".git" in directories or ".git" in files:
                raise ValueError("Contains Git data")
            for directory in directories:
                child = Path(parent) / directory
                if not child.is_symlink() and os.path.ismount(child):
                    raise ValueError("Contains a nested mount")
        return timestamp, allocated_bytes(root)

    def clean(self, apply=False, budget=DEFAULT_BUDGET):
        result = {"candidates": [], "preserved": [], "removed_bytes": 0,
                  "retained_bytes": 0, "busy": False, "apply": apply}
        self.validate_path()
        if not self.base.exists():
            return result
        self.validate()
        try:
            with lock_file(self.base / ".manager.lock"):
                rows = []
                for path in sorted(self.base.iterdir()):
                    if path.name.startswith("."):
                        continue
                    try:
                        with lock_file(path / "lease"):
                            timestamp, size = self.inspect_run(path)
                        rows.append((timestamp, path, size))
                    except (OSError, ValueError, TypeError) as error:
                        result["preserved"].append({"path": str(path), "reason": str(error)})
                retained = sum(row[2] for row in rows)
                for timestamp, path, size in sorted(rows):
                    reason = "expired" if time.time() - timestamp >= RETENTION else "budget"
                    if reason == "budget" and retained <= budget:
                        continue
                    # Recheck while holding the lease throughout deletion.
                    with lock_file(path / "lease") as lease_fd:
                        self.inspect_run(path)
                        result["candidates"].append({"path": str(path), "allocated_bytes": size,
                                                     "reason": reason})
                        if apply:
                            remove_run(path, lease_fd, self.repository, self.inspect_run)
                            result["removed_bytes"] += size
                    retained -= size
                result["retained_bytes"] = retained
        except BlockingIOError:
            result["busy"] = True
        return result

    def run(self, command):
        self.initialize()
        with lock_file(self.base / ".manager.lock"):
            path = self.base / ("run-" + uuid.uuid4().hex)
            path.mkdir()
            work = path / "work"
            work.mkdir()
            (path / "lease").touch()
            metadata = {"owner": OWNER, "repository": str(self.repository),
                        "created": time.time(), "completed": None, "state": "running"}
            write_json(path / "run.json", metadata)
            # Acquire before publishing an unlocked, runnable directory.
            lease = lock_file(path / "lease")
            descriptor = lease.__enter__()
        print(f"[storage] Scratch: {work}", file=sys.stderr)
        environment = dict(os.environ, SIGNAL_FISH_SCRATCH_DIR=str(work), TMPDIR=str(work),
                           CARGO_TARGET_DIR=str(work / "target"),
                           NESTED_CARGO_TARGET_DIR=str(work / "policy-target"))
        status = 127
        child = None
        old_term = signal.getsignal(signal.SIGTERM)

        def interrupted(_signal, _frame):
            raise KeyboardInterrupt

        signal.signal(signal.SIGTERM, interrupted)
        try:
            child = subprocess.Popen(command, cwd=self.repository, env=environment,
                                     pass_fds=(descriptor,), start_new_session=True)
            status = child.wait()
        except KeyboardInterrupt:
            status = 130
            if child is not None:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait()
        except OSError as error:
            print(f"[storage] {error}", file=sys.stderr)
        finally:
            signal.signal(signal.SIGTERM, old_term)
            metadata.update(completed=time.time(), state="complete" if status == 0 else "failed")
            try:
                write_json(path / "run.json", metadata)
            except (OSError, ValueError, TypeError) as error:
                print(f"[storage] Could not save completion metadata: {error}", file=sys.stderr)
            finally:
                # Delete only when all processes holding the inherited lease exit.
                lease.__exit__(None, None, None)
        try:
            if status == 0:
                with lock_file(self.base / ".manager.lock"):
                    with lock_file(path / "lease") as lease_fd:
                        if shutil.rmtree.avoids_symlink_attacks:
                            self.inspect_run(path)
                            remove_run(path, lease_fd, self.repository, self.inspect_run)
            self.clean(apply=True)
        except (OSError, ValueError, TypeError) as error:
            print(f"[storage] Post-run cleanup skipped: {error}", file=sys.stderr)
        return status if status >= 0 else 128 - status


def capture(command, timeout=15):
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
        return {"command": command, "status": result.returncode,
                "stdout": result.stdout, "stderr": result.stderr}
    except (OSError, subprocess.TimeoutExpired, UnicodeError) as error:
        return {"command": command, "error": str(error)}


def docker_records(result, fields):
    """Keep malformed optional inventory local to its collection result."""
    if result.get("status") != 0:
        return None
    try:
        rows = [json.loads(line) for line in result["stdout"].splitlines()]
        for row in rows:
            if not isinstance(row, dict):
                raise ValueError("Docker record is not an object")
            for name, kind in fields.items():
                if not isinstance(row.get(name), kind):
                    raise ValueError(f"Docker record has invalid {name}")
            if "mounts" in fields:
                for mount in row["mounts"]:
                    if (not isinstance(mount, dict) or not isinstance(mount.get("Type"), str)
                            or (mount["Type"] == "volume"
                                and not isinstance(mount.get("Name"), str))):
                        raise ValueError("Docker record has an invalid mount")
        return rows
    except (ValueError, KeyError, TypeError) as error:
        result["error"] = f"Invalid Docker output: {error}"
        return None


def report(repository, docker=False, docker_timeout=20):
    paths = [repository / "target", repository / "node_modules",
             repository / "clients/browser/node_modules", Path.home(), Path("/tmp"),
             Path("/vscode"), Path("/usr/local/cargo"), Path("/usr/local/rustup")]
    measurements = []
    filesystems = []
    for path in paths:
        if not path.exists():
            continue
        measurement = capture(["du", "-sx", "-B1", str(path)])
        measurement["path"] = str(path)
        if measurement.get("status") == 0:
            measurement["allocated_bytes"] = int(measurement["stdout"].split()[0])
        measurements.append(measurement)
        try:
            filesystem = os.statvfs(path)
            filesystems.append({"path": str(path), "device": path.stat().st_dev,
                                "total_bytes": filesystem.f_blocks * filesystem.f_frsize,
                                "available_bytes": filesystem.f_bavail * filesystem.f_frsize})
        except OSError as error:
            filesystems.append({"path": str(path), "error": str(error)})
    result = {"collected_at": time.time(), "repository": str(repository),
              "measurements": measurements, "filesystems": filesystems,
              "mounts": capture(["findmnt", "--json", "-o", "TARGET,SOURCE,FSTYPE"]),
              "target_breakdown": capture(["du", "-x", "--max-depth=1", "-B1",
                                            str(repository / "target")], timeout=20),
              "notes": ["Overlapping paths and shared filesystem totals must not be summed.",
                        "Removed allocated bytes are estimates; verify filesystem and host deltas.",
                        "Warm target warning: 40 GiB; explicit maintenance threshold: 60 GiB."]}
    target = next((x.get("allocated_bytes") for x in measurements
                   if x["path"] == str(repository / "target")), None)
    result["target_budget"] = {"allocated_bytes": target, "warning_bytes": 40 * GIB,
                               "maintenance_bytes": 60 * GIB,
                               "state": "unknown" if target is None else
                               "maintenance" if target >= 60 * GIB else
                               "warning" if target >= 40 * GIB else "ok"}
    if docker:
        commands = {
            "version": ["docker", "version", "--format", "{{json .Server}}"],
            "containers": ["docker", "ps", "-a", "--format", "{{json .}}"],
            "volumes": ["docker", "volume", "ls", "--format", "{{json .}}"],
            "images": ["docker", "image", "ls", "--format", "{{json .}}"],
            "builders": ["docker", "buildx", "ls"],
            "builder_policy": ["docker", "buildx", "inspect", "default"],
            "accounting": ["docker", "system", "df"],
        }
        result["docker"] = {name: capture(command, timeout=docker_timeout)
                            for name, command in commands.items()}
        # Inspect only selected fields. Never record container environments.
        containers = result["docker"]["containers"]
        listed = docker_records(containers, {"ID": str})
        if listed is not None:
            ids = [row["ID"] for row in listed]
            result["docker"]["references"] = []
            for offset in range(0, len(ids), 64):
                result["docker"]["references"].append(capture([
                    "docker", "inspect", "--format",
                    '{"id":{{json .Id}},"image":{{json .Image}},"state":{{json .State.Status}},'
                    '"project":{{json (index .Config.Labels "devcontainer.local_folder")}},'
                    '"mounts":{{json .Mounts}}}', *ids[offset:offset + 64]], timeout=10))
            references = []
            for batch in result["docker"]["references"]:
                rows = docker_records(batch, {"mounts": list})
                if rows is not None:
                    references.extend(rows)
            volumes = result["docker"]["volumes"]
            listed_volumes = docker_records(volumes, {"Name": str})
            if listed_volumes is not None:
                names = {row["Name"] for row in listed_volumes}
                used = {mount["Name"] for row in references for mount in row["mounts"]
                        if mount["Type"] == "volume"}
                result["docker"]["reference_summary"] = {
                    "containers_listed": len(ids), "containers_inspected": len(references),
                    "volumes_listed": len(names), "volumes_referenced": len(used),
                    "unreferenced_in_snapshot": sorted(names - used)
                    if len(references) == len(ids) else None,
                    "note": "Snapshot only. Recheck all live references before any host removal.",
                }
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    commands = parser.add_subparsers(dest="command", required=True)
    reporting = commands.add_parser("report", help="Read-only allocated-space inventory")
    reporting.add_argument("--json", action="store_true")
    reporting.add_argument("--docker", action="store_true", help="Also collect host Docker references")
    reporting.add_argument("--docker-timeout", type=int, default=20,
                           help="Per-query Docker timeout in seconds (1-300; default: 20)")
    cleaning = commands.add_parser("clean", help="Clean only marker-owned scratch runs")
    mode = cleaning.add_mutually_exclusive_group()
    mode.add_argument("--apply", action="store_true")
    mode.add_argument("--dry-run", action="store_true")
    cleaning.add_argument("--budget-gib", type=float, default=20)
    running = commands.add_parser("run-scratch", help="Run an isolated disposable build")
    running.add_argument("args", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    manager = Manager(args.root)
    try:
        if args.command == "run-scratch":
            command = args.args[1:] if args.args[:1] == ["--"] else args.args
            if not command:
                parser.error("run-scratch requires a command after --")
            return manager.run(command)
        if args.command == "clean":
            if not math.isfinite(args.budget_gib) or args.budget_gib < 0:
                parser.error("--budget-gib must be finite and nonnegative")
            result = manager.clean(apply=args.apply, budget=int(args.budget_gib * GIB))
            print(json.dumps(result, indent=2))
        else:
            if not 1 <= args.docker_timeout <= 300:
                parser.error("--docker-timeout must be between 1 and 300 seconds")
            result = report(manager.repository, args.docker, args.docker_timeout)
            if args.json:
                print(json.dumps(result, indent=2))
            else:
                for item in result["measurements"]:
                    size = item.get("allocated_bytes")
                    print(f"{size / GIB:.2f} GiB\t{item['path']}" if size is not None
                          else f"UNKNOWN\t{item['path']}: {item}")
                print("Target budget: " + result["target_budget"]["state"])
                for note in result["notes"]:
                    print(note)
                if args.docker:
                    print(json.dumps(result["docker"], indent=2))
        return 0
    except (OSError, ValueError, TypeError) as error:
        print(f"[storage] {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
