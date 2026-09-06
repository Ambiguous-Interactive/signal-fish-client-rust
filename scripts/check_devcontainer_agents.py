#!/usr/bin/env python3
"""Validate the devcontainer's agent tools, permissions, and MCP wiring."""

from __future__ import annotations

import json
import re
import stat
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import Any


SERVER_ARGS = {
    "github": ["github"],
    "zai-vision": ["zai-vision"],
    "zai-web-search": ["zai-web-search"],
    "zai-web-reader": ["zai-web-reader"],
    "zai-zread": ["zai-zread"],
}

CODEX_ENV_VARS = {
    "github": ["GITHUB_PERSONAL_ACCESS_TOKEN", "GITHUB_TOKEN", "GH_TOKEN", "GITHUB_PAT"],
    "zai-vision": ["Z_AI_API_KEY", "Z_AI_MODE"],
    "zai-web-search": ["Z_AI_API_KEY"],
    "zai-web-reader": ["Z_AI_API_KEY"],
    "zai-zread": ["Z_AI_API_KEY"],
}

NPM_PACKAGES = {
    "@openai/codex@latest",
    "opencode-ai@latest",
    "@nanocollective/nanocoder@latest",
    "@z_ai/mcp-server@latest",
    "mcp-remote@latest",
}

SECRET_PATTERNS = (
    re.compile(r"\bgh[pousr]_[A-Za-z0-9_]{20,}"),
    re.compile(r'(?i)(?:api[_-]?key|token)\s*[=:]\s*["\'](?!\$|\{env:)[^"\']{12,}'),
)


def _strip_jsonc(text: str) -> str:
    """Strip JSONC comments and trailing commas without touching strings."""
    without_comments: list[str] = []
    index = 0
    in_string = False
    escaped = False
    while index < len(text):
        char = text[index]
        if in_string:
            without_comments.append(char)
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
            index += 1
            continue
        if char == '"':
            in_string = True
            without_comments.append(char)
            index += 1
            continue
        if text[index : index + 2] == "//":
            newline = text.find("\n", index + 2)
            index = len(text) if newline == -1 else newline
            continue
        if text[index : index + 2] == "/*":
            close = text.find("*/", index + 2)
            if close == -1:
                raise ValueError("unterminated JSONC block comment")
            index = close + 2
            continue
        without_comments.append(char)
        index += 1

    uncommented = "".join(without_comments)
    result: list[str] = []
    index = 0
    in_string = False
    escaped = False
    while index < len(uncommented):
        char = uncommented[index]
        if in_string:
            result.append(char)
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
            index += 1
            continue
        if char == '"':
            in_string = True
            result.append(char)
            index += 1
            continue
        if char == ",":
            lookahead = index + 1
            while lookahead < len(uncommented) and uncommented[lookahead].isspace():
                lookahead += 1
            if lookahead < len(uncommented) and uncommented[lookahead] in "]}":
                index += 1
                continue
        result.append(char)
        index += 1
    return "".join(result)


def _load_json(path: Path, errors: list[str]) -> dict[str, Any]:
    try:
        value = json.loads(_strip_jsonc(path.read_text(encoding="utf-8")))
    except (OSError, ValueError, json.JSONDecodeError) as error:
        errors.append(f"{path}: cannot parse JSON/JSONC: {error}")
        return {}
    if not isinstance(value, dict):
        errors.append(f"{path}: top-level value must be an object")
        return {}
    return value


def _check_server_map(
    path: Path, servers: Any, command_shape: str, errors: list[str]
) -> None:
    if not isinstance(servers, dict):
        errors.append(f"{path}: missing MCP server object")
        return
    for name, expected_args in SERVER_ARGS.items():
        server = servers.get(name)
        if not isinstance(server, dict):
            errors.append(f"{path}: missing MCP server {name!r}")
            continue
        if command_shape == "array":
            expected_command: Any = ["signal-fish-mcp", *expected_args]
        else:
            expected_command = "signal-fish-mcp"
        if server.get("command") != expected_command:
            errors.append(
                f"{path}: {name!r} command must be {expected_command!r}"
            )
        if command_shape == "scalar" and server.get("args") != expected_args:
            errors.append(f"{path}: {name!r} args must be {expected_args!r}")


def _check_json_configs(root: Path, errors: list[str]) -> None:
    portable_path = root / ".mcp.json"
    portable = _load_json(portable_path, errors)
    _check_server_map(portable_path, portable.get("mcpServers"), "scalar", errors)

    vscode_path = root / ".vscode" / "mcp.json"
    vscode = _load_json(vscode_path, errors)
    _check_server_map(vscode_path, vscode.get("servers"), "scalar", errors)

    opencode_path = root / "opencode.json"
    opencode = _load_json(opencode_path, errors)
    _check_server_map(opencode_path, opencode.get("mcp"), "array", errors)


def _check_codex(root: Path, errors: list[str]) -> None:
    path = root / ".codex" / "config.toml"
    try:
        config = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        errors.append(f"{path}: cannot parse TOML: {error}")
        return
    _check_server_map(path, config.get("mcp_servers"), "scalar", errors)
    for name, required in CODEX_ENV_VARS.items():
        server = config.get("mcp_servers", {}).get(name, {})
        if not isinstance(server, dict):
            continue
        forwarded = server.get("env_vars", [])
        for variable in required:
            if variable not in forwarded:
                errors.append(f"{path}: {name!r} must forward {variable} via env_vars")


def _check_devcontainer(root: Path, errors: list[str]) -> None:
    config_path = root / ".devcontainer" / "devcontainer.json"
    config = _load_json(config_path, errors)
    remote_env = config.get("remoteEnv")
    if not isinstance(remote_env, dict):
        errors.append(f"{config_path}: remoteEnv must be an object")
    else:
        expected_env = {
            "Z_AI_API_KEY": "${localEnv:Z_AI_API_KEY}",
            "Z_AI_MODE": "ZAI",
        }
        for name, expected in expected_env.items():
            if remote_env.get(name) != expected:
                errors.append(f"{config_path}: remoteEnv.{name} must be {expected!r}")

    post_start = config.get("postStartCommand", "")
    if "post-start.sh" not in str(post_start):
        errors.append(f"{config_path}: postStartCommand must run post-start.sh")

    extensions = (
        config.get("customizations", {}).get("vscode", {}).get("extensions", [])
        if isinstance(config.get("customizations"), dict)
        else []
    )
    if "sst-dev.opencode" not in extensions:
        errors.append(f"{config_path}: missing official sst-dev.opencode extension")


def _check_dockerfile(root: Path, errors: list[str]) -> None:
    path = root / ".devcontainer" / "Dockerfile"
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        errors.append(f"{path}: cannot read: {error}")
        return

    node_match = re.search(r"^ARG NODE_VERSION=(\d+)\.", text, re.MULTILINE)
    if node_match is None or int(node_match.group(1)) < 22:
        errors.append(f"{path}: must install a pinned Node.js version >= 22")
    for required in (
        "NODE_X64_SHA256",
        "NODE_ARM64_SHA256",
        "sha256sum -c",
        'NPM_CONFIG_PREFIX="/home/vscode/.local"',
        'NPM_CONFIG_CACHE="/home/vscode/.cache/npm"',
        "/home/vscode/.local/bin",
        "GITHUB_MCP_SERVER_VERSION",
        "GITHUB_MCP_X64_SHA256",
        "GITHUB_MCP_ARM64_SHA256",
        "signal-fish-mcp",
        "install-agent-tools.sh",
    ):
        if required not in text:
            errors.append(f"{path}: missing required build guarantee {required!r}")

    user_position = text.find("USER vscode")
    installer_position = text.find("install-agent-tools.sh", user_position + 1)
    if user_position == -1 or installer_position == -1:
        errors.append(f"{path}: agent npm tools must be installed as vscode")

    owned_dirs = re.search(
        r"RUN install -d -o vscode -g vscode\s+((?:[^\n]*\\\n)*[^\n]*)", text
    )
    if owned_dirs is None or "/home/vscode/.cache" not in owned_dirs.group(1).split():
        errors.append(f"{path}: must explicitly create /home/vscode/.cache owned by vscode")


def _tracked_mode(root: Path, path: Path) -> int | None:
    """Return the git index mode for `path`, or None when not tracked.

    A fresh checkout materializes the index mode, so validating the index —
    not the local disk — is what predicts CI. Disk mode hides a committed
    file whose executable bit was never staged.
    """
    try:
        listing = subprocess.run(
            ["git", "-C", str(root), "ls-files", "-s", "--", str(path)],
            capture_output=True,
            text=True,
            timeout=10,
            check=True,
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    entry = listing.split(" ")
    if len(entry) < 2 or not entry[0].isdigit():
        return None
    return int(entry[0], 8)


def _check_scripts(root: Path, errors: list[str]) -> None:
    installer = root / ".devcontainer" / "scripts" / "install-agent-tools.sh"
    launcher = root / ".devcontainer" / "scripts" / "mcp-launch.sh"
    post_start = root / ".devcontainer" / "scripts" / "post-start.sh"
    for path in (installer, launcher, post_start):
        mode = _tracked_mode(root, path)
        if mode is None:
            try:
                mode = path.stat().st_mode
            except OSError as error:
                errors.append(f"{path}: cannot stat: {error}")
                continue
        if not mode & stat.S_IXUSR:
            errors.append(
                f"{path}: must be executable (git index mode 0{mode:o} "
                "loses the bit on checkout; run `git update-index --chmod=+x`)"
            )

    try:
        installer_text = installer.read_text(encoding="utf-8")
    except OSError:
        installer_text = ""
    for package in sorted(NPM_PACKAGES):
        if package not in installer_text:
            errors.append(f"{installer}: missing {package}")
    if "npm install" not in installer_text or "--global" not in installer_text:
        errors.append(f"{installer}: must install the agent packages globally")

    try:
        launcher_text = launcher.read_text(encoding="utf-8")
    except OSError:
        launcher_text = ""
    for server in SERVER_ARGS:
        if server not in launcher_text:
            errors.append(f"{launcher}: missing launcher for {server!r}")
    for required in (
        "github-mcp-server",
        "git credential fill",
        "zai-mcp-server",
        "mcp-remote",
        "--header-file",
        "Z_AI_API_KEY",
    ):
        if required not in launcher_text:
            errors.append(f"{launcher}: missing secure MCP behavior {required!r}")

    try:
        post_start_text = post_start.read_text(encoding="utf-8")
    except OSError:
        post_start_text = ""
    if "install-agent-tools.sh --update" not in post_start_text:
        errors.append(f"{post_start}: must refresh agent tools on every launch")
    if "sudo git config" in post_start_text:
        errors.append(f"{post_start}: Git setup must not require sudo")


def _check_no_committed_secrets(root: Path, errors: list[str]) -> None:
    paths = [
        root / ".mcp.json",
        root / ".vscode" / "mcp.json",
        root / "opencode.json",
        root / ".codex" / "config.toml",
    ]
    for path in paths:
        try:
            text = path.read_text(encoding="utf-8")
        except OSError:
            continue
        for pattern in SECRET_PATTERNS:
            if pattern.search(text):
                errors.append(f"{path}: appears to contain a committed credential")


def validate(root: Path) -> list[str]:
    errors: list[str] = []
    _check_json_configs(root, errors)
    _check_codex(root, errors)
    _check_devcontainer(root, errors)
    _check_dockerfile(root, errors)
    _check_scripts(root, errors)
    _check_no_committed_secrets(root, errors)
    return errors


def main(argv: list[str]) -> int:
    root = Path(argv[1]).resolve() if len(argv) > 1 else Path(__file__).resolve().parent.parent
    errors = validate(root)
    if errors:
        print("Devcontainer agent integration check failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    print("Devcontainer agent integration check passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
