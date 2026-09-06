#!/usr/bin/env python3
"""Unit tests for check_devcontainer_agents.py."""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("check_devcontainer_agents.py")
SPEC = importlib.util.spec_from_file_location("check_devcontainer_agents", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {MODULE_PATH}")
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


class DevcontainerAgentCheckTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        (self.root / ".devcontainer" / "scripts").mkdir(parents=True)
        (self.root / ".vscode").mkdir()
        (self.root / ".codex").mkdir()
        self._write_valid_fixture()

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    @staticmethod
    def _servers(command_array: bool = False) -> dict[str, object]:
        result: dict[str, object] = {}
        for name, args in CHECKER.SERVER_ARGS.items():
            if command_array:
                result[name] = {"type": "local", "command": ["signal-fish-mcp", *args]}
            else:
                result[name] = {"command": "signal-fish-mcp", "args": args}
        return result

    def _write_valid_fixture(self) -> None:
        (self.root / ".mcp.json").write_text(
            json.dumps({"mcpServers": self._servers()}), encoding="utf-8"
        )
        (self.root / ".vscode" / "mcp.json").write_text(
            json.dumps({"servers": self._servers()}), encoding="utf-8"
        )
        (self.root / "opencode.json").write_text(
            json.dumps({"mcp": self._servers(command_array=True)}), encoding="utf-8"
        )
        codex = "\n".join(
            f'[mcp_servers."{name}"]\ncommand = "signal-fish-mcp"\nargs = {json.dumps(args)}\n'
            f'env_vars = {json.dumps(CHECKER.CODEX_ENV_VARS[name])}'
            for name, args in CHECKER.SERVER_ARGS.items()
        )
        (self.root / ".codex" / "config.toml").write_text(codex, encoding="utf-8")
        devcontainer = {
            "remoteEnv": {
                "Z_AI_API_KEY": "${localEnv:Z_AI_API_KEY}",
                "Z_AI_MODE": "ZAI",
            },
            "customizations": {"vscode": {"extensions": ["sst-dev.opencode"]}},
            "postStartCommand": "bash .devcontainer/scripts/post-start.sh",
        }
        (self.root / ".devcontainer" / "devcontainer.json").write_text(
            json.dumps(devcontainer), encoding="utf-8"
        )
        dockerfile = """ARG NODE_VERSION=22.1.0
ARG NODE_X64_SHA256=x
ARG NODE_ARM64_SHA256=x
RUN sha256sum -c
ENV NPM_CONFIG_PREFIX="/home/vscode/.local" \\
    NPM_CONFIG_CACHE="/home/vscode/.cache/npm" \\
    PATH="/home/vscode/.local/bin:${PATH}"
ARG GITHUB_MCP_SERVER_VERSION=1.0.0
ARG GITHUB_MCP_X64_SHA256=x
ARG GITHUB_MCP_ARM64_SHA256=x
RUN install -d -o vscode -g vscode /home/vscode/.cache /home/vscode/.cache/npm
COPY .devcontainer/scripts/mcp-launch.sh /usr/local/bin/signal-fish-mcp
USER vscode
COPY .devcontainer/scripts/install-agent-tools.sh /usr/local/bin/install-agent-tools.sh
RUN install-agent-tools.sh
"""
        (self.root / ".devcontainer" / "Dockerfile").write_text(
            dockerfile, encoding="utf-8"
        )
        installer = "npm install --global " + " ".join(sorted(CHECKER.NPM_PACKAGES))
        launcher = " ".join(
            [
                *CHECKER.SERVER_ARGS,
                "github-mcp-server",
                "git credential fill",
                "zai-mcp-server",
                "mcp-remote",
                "--header-file",
                "Z_AI_API_KEY",
            ]
        )
        scripts = {
            "install-agent-tools.sh": installer,
            "mcp-launch.sh": launcher,
            "post-start.sh": "install-agent-tools.sh --update",
        }
        for name, contents in scripts.items():
            path = self.root / ".devcontainer" / "scripts" / name
            path.write_text(contents, encoding="utf-8")
            path.chmod(0o755)

    def test_valid_fixture_passes(self) -> None:
        self.assertEqual(CHECKER.validate(self.root), [])

    def test_each_frontend_must_have_every_server(self) -> None:
        path = self.root / ".mcp.json"
        config = json.loads(path.read_text(encoding="utf-8"))
        del config["mcpServers"]["github"]
        path.write_text(json.dumps(config), encoding="utf-8")
        errors = CHECKER.validate(self.root)
        self.assertTrue(any("missing MCP server 'github'" in error for error in errors))

    def test_node_18_and_root_prefix_are_rejected(self) -> None:
        path = self.root / ".devcontainer" / "Dockerfile"
        text = path.read_text(encoding="utf-8")
        text = text.replace("NODE_VERSION=22.1.0", "NODE_VERSION=18.20.4")
        text = text.replace(
            'NPM_CONFIG_PREFIX="/home/vscode/.local"', 'NPM_CONFIG_PREFIX="/usr/local"'
        )
        path.write_text(text, encoding="utf-8")
        errors = CHECKER.validate(self.root)
        self.assertTrue(any("Node.js version >= 22" in error for error in errors))
        self.assertTrue(any("NPM_CONFIG_PREFIX" in error for error in errors))

    def test_committed_token_is_rejected(self) -> None:
        path = self.root / ".mcp.json"
        config = json.loads(path.read_text(encoding="utf-8"))
        config["mcpServers"]["github"]["env"] = {
            "GITHUB_TOKEN": "ghp_abcdefghijklmnopqrstuvwxyz123456"
        }
        path.write_text(json.dumps(config), encoding="utf-8")
        errors = CHECKER.validate(self.root)
        self.assertTrue(any("committed credential" in error for error in errors))

    def test_codex_must_forward_credentials(self) -> None:
        path = self.root / ".codex" / "config.toml"
        text = path.read_text(encoding="utf-8")
        path.write_text(text.replace('"Z_AI_API_KEY"', '"UNRELATED"'), encoding="utf-8")
        errors = CHECKER.validate(self.root)
        self.assertTrue(any("must forward Z_AI_API_KEY" in error for error in errors))

    def test_npm_cache_ownership_is_not_enough(self) -> None:
        path = self.root / ".devcontainer" / "Dockerfile"
        text = path.read_text(encoding="utf-8")
        path.write_text(text.replace("/home/vscode/.cache ", ""), encoding="utf-8")
        errors = CHECKER.validate(self.root)
        self.assertTrue(any("explicitly create /home/vscode/.cache" in error for error in errors))


@unittest.skipUnless(os.name == "posix" and shutil.which("bash"), "requires Linux/bash")
class McpLauncherTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        self.launcher = MODULE_PATH.parent.parent / ".devcontainer/scripts/mcp-launch.sh"
        self.env = {"PATH": f"{self.root}:/usr/bin:/bin", "XDG_RUNTIME_DIR": str(self.root)}
        self.env["CONTAINER_WORKSPACE_FOLDER"] = str(self.root)
        for name in ("node", "zai-mcp-server", "github-mcp-server", "mcp-remote", "signal-fish-mcp"):
            self.command(name, "exit 0\n")

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def command(self, name: str, body: str) -> None:
        path = self.root / name
        path.write_text("#!/bin/sh\n" + body, encoding="utf-8")
        path.chmod(0o755)

    def run_launcher(self, server: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(self.launcher), server], env=self.env,
            capture_output=True, text=True, timeout=10, check=False,
        )

    def test_check_is_offline_and_keeps_stdout_clean(self) -> None:
        # Discover executables without invoking a server or requiring a secret.
        for name in ("node", "zai-mcp-server", "github-mcp-server", "mcp-remote"):
            self.command(name, "exit 99\n")
        result = self.run_launcher("--check")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn("credentials and remote connections not checked", result.stderr)

    def test_missing_credentials_are_actionable(self) -> None:
        for name in ("zai-vision", "zai-web-search", "zai-web-reader", "zai-zread"):
            with self.subTest(server=name):
                result = self.run_launcher(name)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")
                self.assertIn("Z_AI_API_KEY is required", result.stderr)

    def test_remote_header_is_private_removed_and_not_in_arguments(self) -> None:
        self.env["Z_AI_API_KEY"] = "test-only-credential"
        self.command("mcp-remote", '''
test "$1" = "https://api.z.ai/api/mcp/web_search_prime/mcp" || exit 91
test "$2" = "--header-file" || exit 92
test "$(stat -c %a "$3")" = 600 || exit 93
test "$(cat "$3")" = "Authorization: Bearer $Z_AI_API_KEY" || exit 94
test "$#" = 3 || exit 95
exit 7
''')
        result = self.run_launcher("zai-web-search")
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertNotIn(self.env["Z_AI_API_KEY"], result.stderr)
        self.assertEqual(list(self.root.glob("signal-fish-zai-header.*")), [])

    @unittest.skipUnless(shutil.which("node"), "requires Node.js 22+")
    def test_dotenv_parser_regressions(self) -> None:
        result = subprocess.run(
            ["node", "--test", str(MODULE_PATH.with_name("test_mcp_env.cjs"))],
            capture_output=True, text=True, timeout=10, check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    @unittest.skipUnless(shutil.which("node"), "requires Node.js 22+")
    def test_dotenv_credentials_reach_stdio_server(self) -> None:
        node = shutil.which("node")
        (self.root / "node").unlink()
        (self.root / "node").symlink_to(node)
        (self.root / ".env.local").write_text('Z_AI_API_KEY="dotenv-test-key"\n', encoding="utf-8")
        self.env["Z_AI_API_KEY"] = ""  # Mirrors an absent host variable in remoteEnv.
        self.command("zai-mcp-server", '''
test "$Z_AI_API_KEY" = dotenv-test-key || exit 91
test "$Z_AI_MODE" = ZAI || exit 92
read -r line
printf '%s\\n' "$line"
''')
        result = subprocess.run(
            ["bash", str(self.launcher), "zai-vision"], env=self.env,
            input='{"jsonrpc":"2.0","id":1}\n', capture_output=True,
            text=True, timeout=10, check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, '{"jsonrpc":"2.0","id":1}\n')
        self.assertEqual(result.stderr, "")


if __name__ == "__main__":
    unittest.main()
