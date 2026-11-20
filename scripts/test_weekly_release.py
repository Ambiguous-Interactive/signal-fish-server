"""Controls for the weekly release decision and GitHub handoff."""
import subprocess
import unittest
from unittest.mock import patch

import weekly_release as weekly


class WeeklyReleaseTests(unittest.TestCase):
    def test_release_decision(self):
        for state, expected in [
            ((False, False, False), "retry"),
            ((False, True, True), "retry"),
            ((True, True, True), "skip"),
            ((True, False, False), "skip"),
            ((True, False, True), "prepare"),
        ]:
            with self.subTest(state=state):
                self.assertEqual(weekly.action_for(*state), expected)

    def test_changed_pr_identity_blocks_merge(self):
        pr = {
            "state": "open",
            "draft": False,
            "user": {"login": weekly.APP_LOGIN},
            "head": {"sha": "wrong", "ref": weekly_branch, "repo": {"full_name": repo}},
            "base": {"ref": "main", "repo": {"full_name": repo}},
        }
        with self.assertRaises(weekly.ReleaseError):
            weekly.verify_pr(pr, repo, "main", weekly_branch, head)

    def test_missing_ci_cannot_pass(self):
        with patch.object(weekly, "api", return_value={"workflow_runs": []}):
            self.assertFalse(weekly.checks_passed(repo, weekly_branch, head, 7))

    def test_failed_ci_stops_merge(self):
        run = {
            "head_sha": head,
            "head_branch": weekly_branch,
            "event": "pull_request",
            "status": "completed",
            "conclusion": "failure",
            "pull_requests": [{"number": 7}],
        }
        with patch.object(weekly, "api", return_value={"workflow_runs": [run]}):
            with self.assertRaises(weekly.ReleaseError):
                weekly.checks_passed(repo, weekly_branch, head, 7)

    def test_missing_assets_are_retried_and_conflicting_source_is_rejected(self):
        tag = "v0.12.0"
        release = {
            "tag_name": tag,
            "name": tag,
            "draft": False,
            "prerelease": False,
            "body": f"Source revision: `{head}`\nMulti-architecture manifest digest: `sha256:{'c' * 64}`",
            "assets": [{"name": name, "state": "uploaded"} for name in weekly.asset_names(tag)],
        }
        self.assertEqual(len(release["assets"]), 13)
        with patch.object(weekly, "container_digest", return_value=f"sha256:{'c' * 64}") as probe:
            self.assertTrue(weekly.release_complete([release], tag, head, True, True, repo))
            probe.return_value = None
            self.assertFalse(weekly.release_complete([release], tag, head, True, True, repo))
            probe.return_value = f"sha256:{'d' * 64}"
            with self.assertRaises(weekly.ReleaseError):
                weekly.release_complete([release], tag, head, True, True, repo)
            release["assets"].pop()
            self.assertFalse(weekly.release_complete([release], tag, head, True, True, repo))
            with self.assertRaises(weekly.ReleaseError):
                weekly.release_complete([release], tag, "d" * 40, True, True, repo)

    def test_merge_checks_base_and_result(self):
        pr = {
            "state": "open",
            "draft": False,
            "user": {"login": weekly.APP_LOGIN},
            "head": {"sha": head, "ref": weekly_branch, "repo": {"full_name": repo}},
            "base": {"ref": "main", "repo": {"full_name": repo}},
        }
        tree = {"tree": {"sha": "c" * 40}}
        merged = "d" * 40
        for outcome in ("advanced", "denied", "wrong-tree", "green"):
            with (
                self.subTest(outcome=outcome),
                patch.object(weekly, "api") as api,
                patch.object(weekly, "checks_passed", return_value=True),
                patch.object(weekly, "command") as command,
                patch.object(weekly, "output") as output,
                patch.object(weekly, "summary"),
                patch.dict(weekly.os.environ, RELEASE_APP_TOKEN="app-token"),
            ):
                actual_base = merged if outcome == "advanced" else base
                result_tree = {"tree": {"sha": merged}} if outcome == "wrong-tree" else tree
                api.side_effect = [pr, {"object": {"sha": actual_base}}, tree, result_tree]
                command.return_value = f'{{"merged":true,"sha":"{merged}"}}'
                if outcome == "advanced":
                    with self.assertRaises(weekly.ReleaseError):
                        weekly.merge(repo, "main", weekly_branch, base, head, 7)
                    command.assert_not_called()
                elif outcome == "denied":
                    command.side_effect = subprocess.CalledProcessError(1, "gh")
                    with self.assertRaises(subprocess.CalledProcessError):
                        weekly.merge(repo, "main", weekly_branch, base, head, 7)
                elif outcome == "wrong-tree":
                    with self.assertRaises(weekly.ReleaseError):
                        weekly.merge(repo, "main", weekly_branch, base, head, 7)
                else:
                    weekly.merge(repo, "main", weekly_branch, base, head, 7)
                    self.assertIn(f"sha={head}", command.call_args.args)
                    output.assert_called_once_with(source=merged)
                if outcome != "green":
                    output.assert_not_called()

    def test_dispatch_pins_the_reviewed_source(self):
        with (
            patch.object(weekly, "api", return_value={"workflow_run_id": 5, "html_url": "https://example.test/5"}) as api,
            patch.object(weekly, "summary") as summary,
        ):
            weekly.dispatch(repo, "main", head)
            payload = api.call_args.kwargs["payload"]
            self.assertEqual(payload["inputs"], {"expected_source_revision": head})
            self.assertIs(payload["return_run_details"], True)
            self.assertIn("https://example.test/5", summary.call_args.args[0])

    def test_dispatch_requires_run_receipt(self):
        with patch.object(weekly, "api", return_value={"workflow_run_id": 5}):
            with self.assertRaises(weekly.ReleaseError):
                weekly.dispatch(repo, "main", head)


repo = "Ambiguous-Interactive/signal-fish-server"
head = "a" * 40
base = "b" * 40
weekly_branch = "release/v0.13.0-weekly-" + base


if __name__ == "__main__":
    unittest.main()
