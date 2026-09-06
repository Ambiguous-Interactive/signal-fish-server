"""Offline regression tests for credential diagnosis and Codex config migration."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("diagnostic", ROOT / "scripts/check-zai-mcp.py")
diagnostic = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(diagnostic)


class ZaiTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        mock = patch.object(diagnostic, "DEFAULT_ENV_FILE", Path(self.directory.name) / ".env.local")
        mock.start()
        self.addCleanup(mock.stop)

    def test_every_frontend_uses_same_launcher_and_server_names(self):
        for filename, section in [(".mcp.json", "mcpServers"),
                                  (".vscode/mcp.json", "servers"), ("opencode.json", "mcp")]:
            text = (ROOT / filename).read_text()
            config = json.loads("\n".join(line for line in text.splitlines()
                                        if not line.startswith("//")))[section]
            for name in ("vision", "web-search", "web-reader", "zread"):
                entry = config["zai-" + name]
                command = entry["command"]
                if section == "mcp":
                    self.assertEqual(entry["type"], "local")
                    command, args = command[0], command[1:]
                else:
                    self.assertEqual(entry["type"], "stdio")
                    args = entry["args"]
                self.assertEqual(command, "node")
                self.assertEqual(args[-1], name)
                self.assertEqual(args[0].removeprefix("${workspaceFolder}/"),
                                 ".devcontainer/zai-mcp.mjs")
                self.assertNotIn("headers", entry)
                if section == "mcpServers":
                    self.assertEqual(entry["transport"], "stdio")

    def test_env_file_is_data_and_last_assignment_wins(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / ".env.local"
            path.write_text("# fixture\nOTHER=ignored\nZ_AI_API_KEY=old\n"
                            "Z_AI_API_KEY=$(never-execute-this)\n")
            self.assertEqual(diagnostic.read_key_file(path), "$(never-execute-this)")
            path.write_text("OTHER=ignored\n")
            self.assertIsNone(diagnostic.read_key_file(path))

    def test_explicit_env_file_overrides_empty_process_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / ".env.local"
            path.write_text("Z_AI_API_KEY=fake-file-secret\n")
            with patch.dict(os.environ, {"Z_AI_API_KEY": ""}), \
                    patch("sys.argv", ["check", "--env-file", str(path)]), \
                    contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(diagnostic.main(), 0)
                self.assertNotIn("fake-file-secret", output.getvalue())

    def test_missing_and_whitespace_credentials_fail_without_network(self):
        for value in ("", " ", "abc\n", "abc def"):
            with self.subTest(value=repr(value)), patch.dict(os.environ, {"Z_AI_API_KEY": value}), \
                    patch("sys.argv", ["check", "--live"]), \
                    patch.object(diagnostic, "check_stdio") as remote, \
                    contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(diagnostic.main(), 1)
                remote.assert_not_called()
                self.assertIn("FAIL", output.getvalue())

    def test_present_credential_is_not_printed(self):
        with patch.dict(os.environ, {"Z_AI_API_KEY": "fake-test-secret"}), \
                patch("sys.argv", ["check"]), contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertEqual(diagnostic.main(), 0)
            self.assertNotIn("fake-test-secret", output.getvalue())

    def test_auth_error_is_not_an_initialize_response(self):
        self.assertFalse(diagnostic.initialized({"code": 1001, "success": False}))
        self.assertFalse(diagnostic.initialized({"jsonrpc": "2.0", "id": 2, "result": {}}))
        self.assertTrue(diagnostic.initialized({"jsonrpc": "2.0", "id": 1,
                                               "result": {"protocolVersion": "2024-11-05"}}))

    @staticmethod
    def tools_response():
        response = io.BytesIO(b'{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}')
        response.headers = {}
        return response

    def test_remote_json_and_sse_and_auth_error(self):
        success = {"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": "2024-11-05"}}
        for body, content_type, expected in (
            (json.dumps(success), "application/json", True),
            ("data: " + json.dumps(success) + "\n\n", "text/event-stream", True),
            (json.dumps({"code": 1001, "success": False}), "application/json", False),
        ):
            response = io.BytesIO(body.encode())
            response.headers = {"Content-Type": content_type}
            empty = io.BytesIO()
            empty.headers = {}
            with patch.object(diagnostic.urllib.request, "urlopen", side_effect=[response, empty, self.tools_response()]) as opened, \
                    contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(diagnostic.check_remote("zread", "fake-secret"), expected)
                request = opened.call_args_list[0].args[0]
                self.assertEqual(request.get_header("Authorization"), "Bearer fake-secret")
                self.assertNotIn("fake-secret", output.getvalue())

    def test_default_file_overrides_stale_environment_and_empty_fails_closed(self):
        path = diagnostic.DEFAULT_ENV_FILE
        for value, expected in [('"fresh-secret"', 0), ('', 1)]:
            path.write_text(f"Z_AI_API_KEY={value}\n")
            with patch.dict(os.environ, {"Z_AI_API_KEY": "stale-secret"}), \
                    patch("sys.argv", ["check"]), \
                    contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(diagnostic.main(), expected)
                self.assertNotIn("fresh-secret", output.getvalue())
                self.assertNotIn("stale-secret", output.getvalue())

    def test_notification_failure_is_not_reported_as_success(self):
        response = io.BytesIO(json.dumps({"jsonrpc": "2.0", "id": 1,
            "result": {"protocolVersion": "2024-11-05"}}).encode())
        response.headers = {"Mcp-Session-Id": "session-fixture"}
        error = diagnostic.urllib.error.HTTPError("https://example.invalid", 401,
                                                  "secret-must-not-print", {}, None)
        with patch.object(diagnostic.urllib.request, "urlopen", side_effect=[response, error]) as opened, \
                contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertFalse(diagnostic.check_remote("zread", "fake-secret"))
            self.assertIn("notifications/initialized HTTP 401", output.getvalue())
            self.assertNotIn("secret-must-not-print", output.getvalue())
            headers = dict(opened.call_args.args[0].header_items())
            self.assertEqual(headers["Mcp-session-id"], "session-fixture")
            self.assertEqual(headers["Mcp-protocol-version"], "2024-11-05")

    def test_legacy_helper_fails_closed_and_never_prints_key_in_error(self):
        env = {**os.environ, "Z_AI_API_KEY": ""}
        result = subprocess.run(["bash", str(ROOT / ".devcontainer/zai-mcp-headers.sh")],
                                env=env, capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        env["Z_AI_API_KEY"] = 'fake-quote-"-key'
        result = subprocess.run(["bash", str(ROOT / ".devcontainer/zai-mcp-headers.sh")],
                                env=env, capture_output=True, text=True, check=True)
        self.assertEqual(json.loads(result.stdout), {"Authorization": 'Bearer fake-quote-"-key'})
        self.assertEqual(result.stderr, "")

    def test_managed_migration_preserves_user_config_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "config.toml"
            custom = ('[mcp_servers.zai_zread]\nurl = "https://example.invalid/custom"\n'
                      'http_headers_helper = "user-owned-helper"\n')
            config.write_text(custom + '''
# >>> signal-fish zai web search mcp >>>
[mcp_servers.zai_web_search]
url = "https://api.z.ai/api/mcp/web_search_prime/mcp"
http_headers_helper = "/home/vscode/.local/bin/signal-fish-zai-mcp-headers"
# <<< signal-fish zai web search mcp <<<
''')
            env = {**os.environ, "CODEX_HOME": directory, "Z_AI_API_KEY": "fake-test-secret"}
            # Override only helper installation to avoid modifying the real user's home.
            command = '. "$1"; install_zai_mcp_header_helper() { return 0; }; configure_codex_mcp_servers'
            def configure():
                subprocess.run(["bash", "-c", command, "test",
                                str(ROOT / ".devcontainer/lib-agent-tools.sh")],
                               env=env, capture_output=True, check=True)
            configure()
            first = config.read_text()
            self.assertIn(custom, first)
            self.assertEqual(first.count("http_headers_helper"), 1)
            self.assertIn('zai-mcp.mjs', first)
            self.assertIn('[mcp_servers.github]', first)
            self.assertIn('[mcp_servers.zai_vision]', first)
            self.assertNotIn("fake-test-secret", first)
            configure()
            self.assertEqual(config.read_text(), first)


if __name__ == "__main__":
    unittest.main()
