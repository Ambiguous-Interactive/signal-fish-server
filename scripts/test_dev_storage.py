"""Behavior tests for disposable devcontainer storage (no Docker required)."""

import importlib.util
from contextlib import contextmanager
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("dev-storage.py")
spec = importlib.util.spec_from_file_location("dev_storage", SCRIPT)
storage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(storage)


class StorageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        (self.repo / "target").mkdir()
        self.manager = storage.Manager(self.repo)
        self.manager.initialize()

    def run_fixture(self, name="old", age=8, state="failed", size=8192):
        run = self.manager.base / name
        run.mkdir()
        (run / "work").mkdir()
        (run / "work" / "data").write_bytes(b"x" * size)
        (run / "lease").touch()
        timestamp = time.time() - age * 86400
        storage.write_json(run / "run.json", {
            "owner": storage.OWNER, "repository": str(self.repo),
            "created": timestamp, "completed": timestamp if state != "running" else None,
            "state": state,
        })
        return run

    def clean(self, apply=False, budget=20 * 1024**3):
        return self.manager.clean(apply=apply, budget=budget)

    def test_dry_run_is_read_only(self):
        run = self.run_fixture()
        before = {str(p): (p.read_bytes(), p.stat().st_mtime_ns)
                  for p in self.repo.rglob("*") if p.is_file()}
        result = self.clean()
        self.assertEqual([x["path"] for x in result["candidates"]], [str(run)])
        after = {str(p): (p.read_bytes(), p.stat().st_mtime_ns)
                 for p in self.repo.rglob("*") if p.is_file()}
        self.assertEqual(before, after)

    def test_failed_metadata_write_preserves_original_and_cleans_own_temp(self):
        target = self.repo / "state.json"
        other_writer = target.with_suffix(".tmp")
        other_writer.write_text("other writer")
        original = {"saved": True}

        def disk_full(_data, stream, **_kwargs):
            stream.write("{")
            raise OSError(28, "fixture: disk full")

        for failure in ["serialization", "disk full", "replacement"]:
            with self.subTest(failure=failure):
                storage.write_json(target, original)
                with self.assertRaises((TypeError, OSError)):
                    if failure == "serialization":
                        storage.write_json(target, {"cannot_serialize": object()})
                    elif failure == "disk full":
                        with patch.object(storage.json, "dump", side_effect=disk_full):
                            storage.write_json(target, {"new": True})
                    else:
                        with patch.object(Path, "replace", side_effect=PermissionError("fixture")):
                            storage.write_json(target, {"new": True})
                self.assertEqual(json.loads(target.read_text()), original)
                self.assertEqual(other_writer.read_text(), "other writer")
                self.assertEqual(list(self.repo.glob("state.*.tmp")), [])

    def test_expired_run_removed_and_repeat_is_empty(self):
        run = self.run_fixture()
        self.assertGreater(self.clean(apply=True)["removed_bytes"], 0)
        self.assertFalse(run.exists())
        self.assertEqual(self.clean(apply=True)["removed_bytes"], 0)

    def test_budget_evicts_oldest_completed_run(self):
        older = self.run_fixture("older", age=2)
        newer = self.run_fixture("newer", age=1)
        budget = storage.allocated_bytes(newer)
        self.clean(apply=True, budget=budget)
        self.assertFalse(older.exists())
        self.assertTrue(newer.exists())

    def test_active_lease_survives_expiry_and_zero_budget(self):
        run = self.run_fixture()
        with storage.lock_file(run / "lease"):
            result = self.clean(apply=True, budget=0)
        self.assertTrue(run.exists())
        self.assertTrue(result["preserved"])

    def test_interrupted_unlocked_run_expires(self):
        run = self.run_fixture(state="running")
        self.clean(apply=True)
        self.assertFalse(run.exists())

    def test_unknown_directory_is_preserved(self):
        unknown = self.manager.base / "unknown"
        unknown.mkdir()
        self.clean(apply=True, budget=0)
        self.assertTrue(unknown.exists())

    def test_wrong_owner_is_preserved(self):
        run = self.run_fixture()
        metadata = json.loads((run / "run.json").read_text())
        metadata["repository"] = "/other/repository"
        storage.write_json(run / "run.json", metadata)
        self.clean(apply=True, budget=0)
        self.assertTrue(run.exists())

    def test_malformed_metadata_and_extra_files_are_preserved(self):
        for index, value in enumerate([[], {"state": "bad"}, {"owner": storage.OWNER}]):
            run = self.run_fixture(f"invalid-{index}")
            (run / "run.json").write_text(json.dumps(value))
        extra = self.run_fixture("extra")
        (extra / "source.rs").write_text("keep")
        self.clean(apply=True, budget=0)
        self.assertEqual(len(list(self.manager.base.glob("invalid-*"))), 3)
        self.assertTrue((extra / "source.rs").exists())

    def test_lease_and_metadata_symlinks_are_preserved(self):
        outside = self.repo / "outside"
        outside.write_text("keep")
        for index, name in enumerate(["lease", "run.json"]):
            run = self.run_fixture(f"symlink-{index}")
            (run / name).unlink()
            (run / name).symlink_to(outside)
        self.clean(apply=True, budget=0)
        self.assertEqual(outside.read_text(), "keep")
        self.assertEqual(len(list(self.manager.base.glob("symlink-*"))), 2)

    def test_manager_rejects_foreign_root_marker(self):
        (self.manager.base / ".owner.json").write_text('{}')
        with self.assertRaises(ValueError):
            self.clean(apply=True)

    def test_fifo_metadata_is_preserved_without_blocking(self):
        run = self.run_fixture()
        (run / "run.json").unlink()
        os.mkfifo(run / "run.json")
        result = self.invoke("clean", "--apply", "--budget-gib", "0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(run.exists())

    def test_allocated_bytes_counts_hardlinks_once(self):
        run = self.run_fixture()
        before = storage.allocated_bytes(run)
        os.link(run / "work/data", run / "work/linked")
        self.assertEqual(storage.allocated_bytes(run), before)

    def test_recent_failed_run_survives(self):
        run = self.run_fixture(age=1)
        self.clean(apply=True)
        self.assertTrue(run.exists())

    def test_bind_mount_with_escaped_space_is_detected(self):
        line = '10 9 0:3 / /scratch/run/work/with\\040space rw - ext4 /dev/example rw\n'
        with patch.object(Path, 'read_text', return_value=line):
            self.assertTrue(storage.mounted_below(Path('/scratch/run')))
            self.assertFalse(storage.mounted_below(Path('/scratch/other')))

    def test_mounted_run_is_preserved(self):
        run = self.run_fixture()
        with patch.object(storage, 'mounted_below', return_value=True):
            self.clean(apply=True, budget=0)
        self.assertTrue(run.exists())

    def test_symlink_and_embedded_git_are_preserved(self):
        outside = self.repo / "source"
        outside.mkdir()
        (outside / "keep").write_text("keep")
        (self.manager.base / "linked").symlink_to(outside, target_is_directory=True)
        run = self.run_fixture()
        (run / "work" / ".git").write_text("gitdir: elsewhere")
        self.clean(apply=True, budget=0)
        self.assertTrue(run.exists())
        self.assertEqual((outside / "keep").read_text(), "keep")

    def test_inner_symlink_does_not_escape(self):
        outside = self.repo / "keep"
        outside.write_text("keep")
        run = self.run_fixture()
        (run / "work" / "linked").symlink_to(outside)
        self.clean(apply=True)
        self.assertEqual(outside.read_text(), "keep")

    def test_replaced_run_path_cannot_delete_outside_work(self):
        run = self.run_fixture()
        outside = self.repo / "outside"
        (outside / "work").mkdir(parents=True)
        keep = outside / "work" / "source.rs"
        keep.write_text("keep")
        original = storage.remove_run

        def replace_before_removal(path, *args, **kwargs):
            path.rename(self.manager.base / ".moved-run")
            path.symlink_to(outside, target_is_directory=True)
            return original(path, *args, **kwargs)

        with patch.object(storage, "remove_run", side_effect=replace_before_removal):
            with self.assertRaises((OSError, ValueError)):
                self.clean(apply=True)
        self.assertEqual(keep.read_text(), "keep")

    def test_replacement_after_open_cannot_redirect_deletion(self):
        run = self.run_fixture()
        outside = self.repo / "outside"
        (outside / "work").mkdir(parents=True)
        keep = outside / "work" / "source.rs"
        keep.write_text("keep")
        original = storage.shutil.rmtree
        swapped = False

        def replace_during_removal(path, *args, **kwargs):
            nonlocal swapped
            if not swapped:
                swapped = True
                run.rename(self.manager.base / ".moved-run")
                run.symlink_to(outside, target_is_directory=True)
            return original(path, *args, **kwargs)

        # Keep the platform capability attribute while wrapping the real remover.
        replace_during_removal.avoids_symlink_attacks = True
        with patch.object(storage.shutil, "rmtree", replace_during_removal):
            with self.assertRaises((OSError, ValueError)):
                self.clean(apply=True)
        self.assertEqual(keep.read_text(), "keep")

    def test_symlinked_target_is_rejected(self):
        other = self.repo / "other"
        other.mkdir()
        alias = self.repo / "alias"
        alias.mkdir()
        (alias / "target").symlink_to(other, target_is_directory=True)
        with self.assertRaises(ValueError):
            storage.Manager(alias).initialize()

    def test_shared_manager_lock_skips_cleanup(self):
        run = self.run_fixture()
        with storage.lock_file(self.manager.base / ".manager.lock"):
            result = self.clean(apply=True)
        self.assertTrue(run.exists())
        self.assertTrue(result["busy"])

    def invoke(self, *args):
        return subprocess.run([sys.executable, str(SCRIPT), "--root", str(self.repo),
                               *args], capture_output=True, text=True, timeout=15)

    def test_successful_command_cleans_work_and_sets_environment(self):
        result = self.invoke("run-scratch", "--", sys.executable, "-c",
                             "import os,pathlib; p=pathlib.Path(os.environ['SIGNAL_FISH_SCRATCH_DIR']); "
                             "assert os.environ['CARGO_TARGET_DIR']==str(p/'target'); "
                             "assert os.environ['TMPDIR']==str(p); (p/'proof').write_text('ok')")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(list(self.manager.base.glob("run-*")), [])

    def test_failed_command_is_retained_with_exit_status(self):
        result = self.invoke("run-scratch", "--", sys.executable, "-c", "raise SystemExit(7)")
        self.assertEqual(result.returncode, 7, result.stderr)
        runs = list(self.manager.base.glob("run-*"))
        self.assertEqual(len(runs), 1)
        self.assertEqual(json.loads((runs[0] / "run.json").read_text())["state"], "failed")

    def test_missing_command_is_retained(self):
        result = self.invoke("run-scratch", "--", "signal-fish-command-that-does-not-exist")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(list(self.manager.base.glob("run-*"))), 1)

    def test_success_status_survives_busy_post_run_cleanup(self):
        original = storage.lock_file
        acquisitions = 0

        @contextmanager
        def busy_after_start(path):
            nonlocal acquisitions
            if path.name == ".manager.lock":
                acquisitions += 1
                if acquisitions > 1:
                    raise BlockingIOError("fixture: cleaner holds manager lock")
            with original(path) as descriptor:
                yield descriptor

        with patch.object(storage, "lock_file", side_effect=busy_after_start):
            self.assertEqual(self.manager.run([sys.executable, "-c", "pass"]), 0)

    def test_failure_status_survives_post_run_cleanup_error(self):
        with patch.object(self.manager, "clean", side_effect=ValueError("fixture: invalid marker")):
            self.assertEqual(self.manager.run([sys.executable, "-c", "raise SystemExit(7)"]), 7)

    def test_status_survives_completion_metadata_error(self):
        original = storage.write_json

        def fail_completion(path, metadata):
            if metadata.get("state") == "complete":
                raise OSError("fixture: cannot save completed state")
            original(path, metadata)

        with patch.object(storage, "write_json", side_effect=fail_completion):
            self.assertEqual(self.manager.run([sys.executable, "-c", "pass"]), 0)

    def test_workflow_stops_on_storage_test_failure(self):
        workflow = (SCRIPT.parents[1] / ".github/workflows/devcontainer-build.yml").read_text()
        lines = workflow.split("runCmd: |\n", 1)[1].splitlines()
        script = []
        for line in lines:
            if line and not line.startswith("            "):
                break
            script.append(line[12:])
        script = "\n".join(script)
        script = script.replace("python3 scripts/test_dev_storage.py", "false")
        script = script.replace("bash .devcontainer/post-start.sh", "touch continued")
        script = script.replace("bash .devcontainer/verify-agent-tooling-runtime.sh", "true")
        script = script.replace("pwsh -NoLogo -NoProfile -NonInteractive -File scripts/test-dev-storage-host.ps1", "true")
        result = subprocess.run(["bash", "-c", script], cwd=self.repo,
                                capture_output=True, text=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.repo / "continued").exists())

    def test_running_command_is_leased_and_termination_retains_failure(self):
        ready = self.repo / "ready"
        child = subprocess.Popen([
            sys.executable, str(SCRIPT), "--root", str(self.repo), "run-scratch", "--",
            sys.executable, "-c",
            "import pathlib,time; pathlib.Path('ready').touch(); time.sleep(30)",
        ], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            deadline = time.monotonic() + 5
            while not ready.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(ready.exists())
            result = self.clean(apply=True, budget=0)
            self.assertEqual(result["candidates"], [])
            child.send_signal(signal.SIGTERM)
            self.assertEqual(child.wait(timeout=5), 130)
            run = next(self.manager.base.glob("run-*"))
            self.assertEqual(json.loads((run / "run.json").read_text())["state"], "failed")
            self.clean(apply=True, budget=0)
            self.assertFalse(run.exists())
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()

    def lifecycle(self):
        directory = self.repo / ".devcontainer"
        directory.mkdir()
        shutil.copy(SCRIPT.parents[1] / ".devcontainer/post-start.sh", directory)
        (directory / "lib-agent-tools.sh").write_text('is_truthy() { [ "$1" = 1 ]; }\n')
        (self.repo / "scripts").mkdir()
        shutil.copy(SCRIPT, self.repo / "scripts/dev-storage.py")
        environment = dict(os.environ, SIGNAL_FISH_SKIP_AGENT_REFRESH="1")
        environment.pop("SIGNAL_FISH_SKIP_STORAGE_CLEANUP", None)
        return subprocess.run(["bash", str(directory / "post-start.sh")],
                              env=environment,
                              capture_output=True, text=True, timeout=12)

    def test_lifecycle_cleans_even_when_agent_refresh_is_disabled(self):
        run = self.run_fixture()
        result = self.lifecycle()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(run.exists())

    def test_lifecycle_ownership_failure_does_not_block_attach(self):
        (self.manager.base / ".owner.json").write_text('{}')
        result = self.lifecycle()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Warning: scratch cleanup did not finish", result.stdout)

    def test_report_has_no_side_effects_in_fresh_checkout(self):
        with tempfile.TemporaryDirectory() as root:
            result = subprocess.run([sys.executable, str(SCRIPT), "--root", root,
                                     "report", "--json"], capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(result.stdout)
            self.assertIn("filesystems", report)
            self.assertIn("measurements", report)
            self.assertEqual(list(Path(root).iterdir()), [])

    def test_malformed_docker_data_preserves_the_rest_of_the_report(self):
        for stage, malformed in [
            ("containers", "not JSON"),
            ("containers", '["not an object"]'),
            ("volumes", '{"Name": []}'),
            ("references", '{"mounts": [null]}'),
        ]:
            with self.subTest(stage=stage, malformed=malformed):
                def fake_capture(command, timeout=15):
                    output = ""
                    kind = None
                    if command[:3] == ["docker", "ps", "-a"]:
                        kind, output = "containers", '{"ID": "example"}'
                    elif command[:3] == ["docker", "volume", "ls"]:
                        kind, output = "volumes", '{"Name": "data"}'
                    elif command[:2] == ["docker", "inspect"]:
                        kind, output = "references", '{"mounts": []}'
                    elif command[0] == "du":
                        output = "4096\tfixture\n"
                    return {"status": 0, "stdout": malformed if kind == stage else output,
                            "stderr": "", "command": command}

                with patch.object(storage, "capture", side_effect=fake_capture):
                    result = storage.report(self.repo, docker=True)
                self.assertTrue(result["measurements"])
                affected = (result["docker"]["references"][0] if stage == "references"
                            else result["docker"][stage])
                self.assertIn("error", affected)


if __name__ == "__main__":
    unittest.main()
