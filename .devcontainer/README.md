# Signal Fish Client SDK - Dev Container

This directory contains the development container configuration for the Signal Fish Client SDK.

## Quick Start

1. Install [Docker Desktop](https://www.docker.com/products/docker-desktop/) and [VS Code](https://code.visualstudio.com/)
2. Install the [Dev Containers extension](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-containers)
3. Open this repository in VS Code
4. Click "Reopen in Container" when prompted (or use Command Palette: `Dev Containers: Reopen in Container`)

Docker must support BuildKit because the Dockerfile uses cache mounts for APT
and Cargo downloads. Current Docker Desktop releases enable this by default.

## What's Included

### Rust Toolchain

- Latest stable Rust with `rust-src` component
- `mold` linker for faster builds
- Full feature support including WebSocket transport

### Cargo Extensions

| Tool | Purpose |
|------|---------|
| `cargo-watch` | Auto-rebuild on file changes |
| `cargo-edit` | Add/remove/upgrade dependencies |
| `cargo-audit` | Security vulnerability checks |
| `cargo-deny` | License and dependency policy checks |
| `cargo-outdated` | Find outdated dependencies |
| `cargo-expand` | Macro expansion viewer |
| `cargo-bloat` | Binary size profiler |
| `cargo-nextest` | Fast test runner |
| `cargo-tarpaulin` | Code coverage |
| `cargo-machete` | Unused dependency finder |

Cargo extension installation is **best-effort** during image build. If a specific crate version fails to compile or fetch (for example due upstream dependency breakage), the build continues and prints a warning so the devcontainer still opens reliably.

### CLI Tools

- `ripgrep` (`rg`) - Fast text search
- `fd` - Fast file finder
- `bat` - Syntax-highlighted cat
- `eza` - Modern ls replacement
- `fzf` - Fuzzy finder
- `delta` - Enhanced git diffs
- `gh` - GitHub CLI
- `jq` - JSON processor

### Agentic Coding Frontends

The image installs current releases of the following CLIs as the unprivileged
`vscode` user:

- OpenAI Codex (`codex`)
- Claude Code (`claude`)
- GitHub Copilot CLI (`copilot`)
- OpenCode (`opencode`)
- Nanocoder (`nanocoder`)

Node.js 22 is checksum-pinned for compatibility with Nanocoder and Z.AI's MCP
server. Image creation installs `@latest` releases; each container start checks
the registry versions in parallel and only runs npm when an update exists. If
the registry is temporarily unavailable, startup continues with the complete
versions already baked into the image.

Global npm packages use `/home/vscode/.local` and the npm cache uses
`/home/vscode/.cache/npm`, both owned by `vscode`. Both `npm install` in a
project and `npm install --global <package>` therefore work without `sudo`.

### MCP Servers

Codex, Claude Code, GitHub Copilot CLI, VS Code/Copilot Chat, OpenCode, and
Nanocoder are preconfigured with the same servers:

| Server | Capability |
|--------|------------|
| `github` | Official GitHub repositories, issues, pull requests, and Actions tools |
| `zai-vision` | Z.AI image, screenshot, diagram, chart, and video understanding |
| `zai-web-search` | Z.AI real-time web search |
| `zai-web-reader` | Z.AI webpage extraction and structured reading |
| `zai-zread` | Z.AI open-source repository documentation and code reading |

The committed project configs are `.codex/config.toml`, `.mcp.json`,
`.vscode/mcp.json`, and `opencode.json`. They contain no credentials. A shared
stdio launcher adapts the same definitions to every frontend, so the setup
survives image rebuilds and fresh clones without rewriting home-directory
configuration.

Put credentials in the repository root's gitignored `.env.local`:

```dotenv
Z_AI_API_KEY=your-zai-key
# Optional if GitHub credentials are not already available through VS Code:
GITHUB_PERSONAL_ACCESS_TOKEN=your-github-token
```

Every configured frontend uses the same launcher, which reads `.env.local`
at server startup. Restart the frontend's MCP servers after editing it; no
image rebuild is needed for credential changes. Dotenv quoting and comments
are supported; shell commands and variable expansion are not evaluated.
Only the Z.AI/GitHub credential variables and `Z_AI_MODE` are loaded.
The file is excluded from Git and the Docker build context.

Alternatively, set `Z_AI_API_KEY` in the environment that launches VS Code
(or as a Codespaces secret). The devcontainer forwards it at runtime;
nonempty inherited values take precedence over `.env.local`, while empty
values fall back to the file. Restart the container after changing host
environment variables. CLI launches outside VS Code locate `.env.local`
through the current Git repository; Dev Containers provides the workspace
path explicitly for extension hosts.

GitHub authentication is automatic when `GITHUB_PERSONAL_ACCESS_TOKEN`,
`GITHUB_TOKEN`, `GH_TOKEN`, or `GITHUB_PAT` is already present. Otherwise the
launcher reuses VS Code's Git credential helper. If neither is available, the
official GitHub MCP server starts its browser/device OAuth flow on first use.
Each frontend may ask once to trust the committed project MCP configuration.

#### Recovering from `program not found`

The MCP configs launch `signal-fish-mcp`, which is installed **inside the
devcontainer image** along with Node and the server binaries. After pulling
changes to the Dockerfile or agent installation scripts, run **Dev Containers:
Rebuild Container** in VS Code, then restart your agent in that container's
terminal. Restarting an old container or updating npm packages does not apply
Dockerfile changes. These configs do not install servers on the Windows,
macOS, or Linux host; a frontend running on the host needs its own MCP setup.

Run this inside the container to check the installation without exposing
credentials or making network requests:

```shell
signal-fish-mcp --check
```

If the launcher itself is missing, the workspace copy can diagnose the old
image:

```shell
bash .devcontainer/scripts/mcp-launch.sh --check
```

The image build and startup hook also check the executables. A passing check
only confirms local installation; use the frontend's MCP status view (for
example, Codex `/mcp`) to verify authentication and tool discovery. Codex's
configuration explicitly forwards the Z.AI/GitHub credential environment
variables and allows 60 seconds for MCP startup. A missing `Z_AI_API_KEY`
produces a credential error after startup reaches the launcher, rather than
`program not found`.

To verify all five servers with your credentials, run:

```shell
python3 scripts/check_mcp_servers.py
```

This performs MCP initialization and tool discovery through the configured
launcher. It does not call any tools or print credentials/server logs, and
returns a failure status if any server cannot connect within its timeout.

### VS Code Extensions

Pre-configured extensions for Rust development, debugging, GitHub integration, and code quality.

## Shell Aliases

Common cargo commands have short aliases:

```bash
# Build/test
ct    # cargo test
cb    # cargo build
cr    # cargo run
cc    # cargo check
cf    # cargo fmt
cl    # cargo clippy
cn    # cargo nextest run

# With all features
cta   # cargo test --all-features
cba   # cargo build --all-features
cla   # cargo clippy --all-targets --all-features -- -D warnings

# Full check (matches CLAUDE.md workflow)
ccheck-all  # fmt + clippy + test
```

## Volume Mounts

The container uses named volumes for caching:

- `signal-fish-cargo-registry` - Cargo package cache
- `signal-fish-cargo-git` - Git dependency cache
- `signal-fish-target-*` - Build artifacts

This devcontainer intentionally does **not** bind-mount host credential paths
such as `~/.ssh`, `~/.gitconfig`, or `~/.gnupg`. Required host-home bind mounts
are fragile across Windows, macOS, Linux, WSL, remote Docker hosts, and
Codespaces because the source path must exist and be shared with Docker before
the container can start.

VS Code Dev Containers already copies local Git configuration and forwards a
running SSH agent. Keep machine-specific extra mounts in a personal local
override instead of committing them to this repository.

## Git Credentials

For HTTPS remotes, configure a credential helper on the host. VS Code reuses it
inside the container.

For SSH remotes, run an SSH agent on the host and add your key:

```bash
ssh-add
```

VS Code forwards the agent socket into the container automatically.

## Troubleshooting

### macOS Users

If you experience workspace mount failures or slow performance:

1. Open Docker Desktop → Settings → Resources → File Sharing
2. Ensure the repository's parent directory is in the shared paths
3. Consider using "VirtioFS" for better performance (Settings → General → "Use VirtioFS")

### Windows Users

**WSL 2 is required.** Native Windows Docker Desktop (without WSL 2 backend) is NOT supported.

This is a Linux container. To use on Windows:

1. Install [WSL 2](https://docs.microsoft.com/en-us/windows/wsl/install)
2. Configure Docker Desktop to use the WSL 2 backend (Settings → General → "Use the WSL 2 based engine")
3. Clone this repository **inside WSL** (e.g., `~/projects/`) for best performance
4. Open VS Code from within WSL (`code .`) or use VS Code's "Remote - WSL" extension

### Mount Failures

The committed devcontainer only uses Docker named volumes for caches, so it
should not fail because host credential files are missing. If you add personal
bind mounts locally, ensure each source path exists and is shared with Docker.

### Startup Reliability

The `postCreateCommand` lifecycle hook runs `cargo fetch` as best-effort — if it fails,
the container still starts normally.

This prevents transient network or registry issues from blocking container startup.
Run `cargo fetch` manually if you want strict verification.

Cargo extension installs during image build are also best-effort. If a specific extension
tool is missing, install it manually inside the container with `cargo install --locked <tool>`.

The required agent CLIs and MCP bridges are installed strictly during image
creation, so a successfully built image is complete. Launch-time updates are
best-effort and preserve those working image-installed versions on network
failure. Registry checks are parallel and do not reinstall unchanged packages.

To verify the complete setup without contacting model providers, run:

```bash
python3 scripts/check_devcontainer_agents.py
codex --version
claude --version
copilot --version
opencode --version
nanocoder --version
github-mcp-server --version
```

### Rebuild Reports "removal is already in progress"

VS Code can briefly race its own rebuild cleanup when Docker takes several
seconds to remove the previous container. The Remote Containers log then ends
before the image build with an error like:

```text
Error response from daemon: removal of container <id> is already in progress
```

This message is not a Dockerfile, mount, or lifecycle-hook failure. Wait for the
first removal to finish, then select **Retry** or run **Dev Containers: Rebuild
Container** once more. The repository's named Cargo and `target` volumes are not
removed by this cleanup.

If the same container ID is still listed after a minute, check its state with
`docker ps -a` and restart Docker Desktop before retrying. Do not run concurrent
rebuild commands for the same workspace.

### Slow Builds

- The mold linker is pre-configured for faster linking
- First build downloads dependencies; subsequent builds use the cached volumes
- If builds are still slow, check Docker resource allocation in Docker Desktop settings

### Commit Signing

#### Option 1: SSH Signing

Use SSH key signing with the forwarded SSH agent:

```bash
git config --global gpg.format ssh
git config --global user.signingkey ~/.ssh/id_ed25519.pub
```

#### Option 2: GPG Signing

Configure GPG on the host and let VS Code Dev Containers share it with the
container. Avoid committing a direct `~/.gnupg` bind mount; it is platform- and
machine-specific and can block the container from opening.

### rust-analyzer Issues

If rust-analyzer shows errors:

1. Reload the window: `Developer: Reload Window`
2. Check for workspace errors: `rust-analyzer: Status`
3. Restart rust-analyzer: `rust-analyzer: Restart server`

## Customization

### Light Theme Users

If using a light VS Code theme, add to `containerEnv` in `devcontainer.json`:

```json
"COLORSCHEME": "light"
```

This configures `delta` (git diff viewer) for light backgrounds.

### Adding Extensions

Add extensions to the `extensions` array in `devcontainer.json`. The extension ID format is `publisher.extensionName`.

### Environment Variables

Add custom environment variables to `containerEnv` in `devcontainer.json`.

## Rebuilding

After modifying `Dockerfile` or `devcontainer.json`:

1. Command Palette: `Dev Containers: Rebuild Container`
2. Or: `Dev Containers: Rebuild Without Cache` for a clean rebuild

A normal rebuild may reuse the cached `@latest` npm layer; the first
`postStartCommand` version check closes that cache gap. Use **Rebuild Without
Cache** only when diagnosing an image-build problem, not for routine agent
updates.
