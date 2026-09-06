#!/usr/bin/env python3
"""Check configured MCP initialization/tools/list without calling any tools.

Run inside the devcontainer. Never print server stderr or credential contents.
"""

from __future__ import annotations

import concurrent.futures
import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import sys
import threading
import time
import tomllib


def check_server(name: str, config: dict) -> bool:
    messages: queue.Queue[str | None] = queue.Queue()
    try:
        process = subprocess.Popen(
            [config["command"], *config.get("args", [])],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True, start_new_session=True,
        )
    except OSError:
        print(f"FAIL {name}: could not launch; run signal-fish-mcp --check", flush=True)
        return False

    def read_stdout() -> None:
        assert process.stdout is not None
        for line in process.stdout:
            messages.put(line)
        messages.put(None)

    threading.Thread(target=read_stdout, daemon=True).start()

    def send(message: dict) -> None:
        assert process.stdin is not None
        process.stdin.write(json.dumps(message) + "\n")
        process.stdin.flush()

    def receive(request_id: int) -> dict:
        deadline = time.monotonic() + 60
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError
            line = messages.get(timeout=remaining)
            if line is None:
                raise EOFError
            message = json.loads(line)
            if message.get("id") == request_id:
                if "error" in message:
                    raise ValueError("MCP error")
                return message["result"]

    try:
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "signal-fish-mcp-check", "version": "1.0"},
        }})
        receive(1)
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        send({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
        result = receive(2)
        count = len(result["tools"])
        if not count:
            raise ValueError("No tools")
        print(f"PASS {name}: initialized; {count} tools discovered", flush=True)
        return True
    except (OSError, EOFError, ValueError, KeyError, TypeError, TimeoutError, queue.Empty):
        print(f"FAIL {name}: initialization/tool discovery failed; check frontend MCP status", flush=True)
        return False
    finally:
        # Terminate the whole launcher/bridge tree, including on timeouts.
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        for stream in (process.stdin, process.stdout):
            if stream is not None:
                stream.close()


def main() -> int:
    if sys.platform != "linux":
        print("Run this check inside the Linux devcontainer.", file=sys.stderr)
        return 1
    root = Path(__file__).resolve().parent.parent
    os.chdir(root)
    os.environ["CONTAINER_WORKSPACE_FOLDER"] = str(root)
    with (root / ".codex/config.toml").open("rb") as source:
        servers = tomllib.load(source)["mcp_servers"]
    with concurrent.futures.ThreadPoolExecutor(max_workers=5) as executor:
        results = list(executor.map(lambda item: check_server(*item), servers.items()))
    return 0 if all(results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
