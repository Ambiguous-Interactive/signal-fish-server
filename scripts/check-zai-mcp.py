#!/usr/bin/env python3
"""Check Z.AI credentials and, with --live, initialize all four MCP servers.

Never print credentials, headers, raw server responses, or child stderr.
Initialization does not invoke any billable search/vision tools.
"""
import argparse
import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_ENV_FILE = ROOT / ".env.local"

ENDPOINTS = ("web_search_prime", "web_reader", "zread")
INITIALIZE = {
    "jsonrpc": "2.0", "id": 1, "method": "initialize",
    "params": {"protocolVersion": "2024-11-05", "capabilities": {},
               "clientInfo": {"name": "signal-fish-diagnostic", "version": "1"}},
}


def initialized(message):
    return (isinstance(message, dict) and message.get("jsonrpc") == "2.0"
            and message.get("id") == 1
            and isinstance(message.get("result"), dict)
            and "protocolVersion" in message["result"])


def check_remote(name, key):
    headers = {"Authorization": f"Bearer {key}", "Content-Type": "application/json",
               "Accept": "application/json, text/event-stream"}
    def send(message):
        request = urllib.request.Request(
            f"https://api.z.ai/api/mcp/{name}/mcp",
            data=json.dumps(message).encode(), headers=headers)
        with urllib.request.urlopen(request, timeout=15) as response:
            if response.headers.get("Mcp-Session-Id"):
                headers["Mcp-Session-Id"] = response.headers["Mcp-Session-Id"]
            if "id" not in message:
                return None
            if "text/event-stream" in response.headers.get("Content-Type", ""):
                for line in response:
                    if line.startswith(b"data:"):
                        value = json.loads(line[5:])
                        if value.get("id") == message["id"]:
                            return value
                return None
            return json.load(response)
    stage = "initialize"
    try:
        result = send(INITIALIZE)
        if not initialized(result):
            print(f"{name}: non-MCP response (check credentials and Coding Plan access)")
            return False
        headers["MCP-Protocol-Version"] = result["result"]["protocolVersion"]
        stage = "notifications/initialized"
        send({"jsonrpc": "2.0", "method": stage})
        stage = "tools/list"
        result = send({"jsonrpc": "2.0", "id": 2, "method": stage, "params": {}})
        return isinstance(result, dict) and isinstance(result.get("result", {}).get("tools"), list)
    except urllib.error.HTTPError as error:
        print(f"{name}: {stage} HTTP {error.code}")
    except (OSError, ValueError):
        print(f"{name}: {stage} network or invalid response failure")
    return False


def check_stdio(name, env_file):
    """Exercise the exact launcher used by frontends through tools/list."""
    try:
        with subprocess.Popen(
            ["node", str(ROOT / ".devcontainer/zai-mcp.mjs"), name, str(env_file)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        ) as child:
            try:
                buffer = b""
                def send(message):
                    child.stdin.write(json.dumps(message).encode() + b"\n")
                    child.stdin.flush()
                def receive(request_id):
                    nonlocal buffer
                    deadline = time.monotonic() + 25
                    with selectors.DefaultSelector() as selector:
                        selector.register(child.stdout, selectors.EVENT_READ)
                        while time.monotonic() < deadline:
                            while b"\n" in buffer:
                                line, buffer = buffer.split(b"\n", 1)
                                message = json.loads(line)
                                if message.get("id") == request_id:
                                    return message
                            if not selector.select(max(0, deadline - time.monotonic())):
                                break
                            chunk = os.read(child.stdout.fileno(), 65536)
                            if not chunk:
                                break
                            buffer += chunk
                    return {}
                send(INITIALIZE)
                if not initialized(receive(1)):
                    return False
                send({"jsonrpc": "2.0", "method": "notifications/initialized"})
                send({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
                return isinstance(receive(2).get("result", {}).get("tools"), list)
            finally:
                child.terminate()
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
    except (OSError, ValueError):
        return False


def read_key_file(path):
    """Use Node's dotenv parser, matching the shared launcher; never source code."""
    result = subprocess.run(
        ["node", "--input-type=module", "-e",
         "import {readFileSync} from 'node:fs'; import {parseEnv} from 'node:util'; "
         "process.stdout.write(JSON.stringify(parseEnv(readFileSync(process.argv[1], 'utf8')).Z_AI_API_KEY ?? null))",
         str(path)], capture_output=True, text=True, check=True)
    return json.loads(result.stdout)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live", action="store_true", help="initialize servers over the network")
    parser.add_argument("--env-file", type=Path, help="override the default repository .env.local file")
    args = parser.parse_args()
    key = os.environ.get("Z_AI_API_KEY", "")
    env_file = args.env_file or DEFAULT_ENV_FILE
    if args.env_file or env_file.exists():
        try:
            file_key = read_key_file(env_file)
            if file_key is not None:
                key = file_key
        except (OSError, UnicodeError, subprocess.CalledProcessError):
            print("FAIL: cannot read the specified env file.")
            return 1
        os.environ["Z_AI_API_KEY"] = key
    if not key.strip():
        print("FAIL: Z_AI_API_KEY is missing or empty in this process.")
        print("Set it in .env.local, then restart MCP servers.")
        print("A key set only in one terminal or a model provider's settings is not shared with other front ends.")
        return 1
    if key.startswith(("\"", "'")) or key != key.strip() or any(c.isspace() for c in key):
        print("FAIL: Z_AI_API_KEY contains quotes or whitespace; use unquoted KEY=value env-file syntax.")
        return 1
    print("Z_AI_API_KEY is present (value hidden).")
    if not args.live:
        print("Use --live to verify authenticated MCP initialization.")
        return 0
    results = {name: check_stdio(name, env_file)
               for name in ("vision", "web-search", "web-reader", "zread")}
    for name, ok in results.items():
        print(f"{name}: {'PASS' if ok else 'FAIL'} initialize + initialized + tools/list")
    return 0 if all(results.values()) else 1


if __name__ == "__main__":
    sys.exit(main())
