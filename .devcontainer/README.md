# Signal Fish Client SDK - Dev Container

This directory contains the development container configuration for the Signal Fish Client SDK.

## Quick Start

1. Install [Docker Desktop](https://www.docker.com/products/docker-desktop/) and [VS Code](https://code.visualstudio.com/)
2. Install the [Dev Containers extension](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-containers)
3. Open this repository in VS Code
4. Click "Reopen in Container" when prompted (or use Command Palette: `Dev Containers: Reopen in Container`)

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

Host directories are mounted read-only:
- `~/.ssh` - SSH keys for git operations
- `~/.gitconfig` - Git configuration
- `~/.gnupg` - GPG keys (read-only; see GPG Signing section)

## Prerequisites

Before opening the devcontainer, ensure these files/directories exist on your host:

```bash
mkdir -p ~/.ssh ~/.gnupg
touch ~/.gitconfig
```

These are mounted into the container for git operations and commit signing.

## Troubleshooting

### macOS Users

If you experience mount failures or slow performance:
1. Open Docker Desktop → Settings → Resources → File Sharing
2. Ensure your home directory (or at least `~/.ssh`, `~/.gnupg`) is in the shared paths
3. Consider using "VirtioFS" for better performance (Settings → General → "Use VirtioFS")

### Windows Users

**WSL 2 is required.** Native Windows Docker Desktop (without WSL 2 backend) is NOT supported.

This is a Linux container. To use on Windows:
1. Install [WSL 2](https://docs.microsoft.com/en-us/windows/wsl/install)
2. Configure Docker Desktop to use the WSL 2 backend (Settings → General → "Use the WSL 2 based engine")
3. Clone this repository **inside WSL** (e.g., `~/projects/`) for best performance
4. Open VS Code from within WSL (`code .`) or use VS Code's "Remote - WSL" extension

### Mount Failures

If the container fails to start due to mount issues, see the [Prerequisites](#prerequisites) section above.

### Startup Reliability

Devcontainer lifecycle hooks are configured as best-effort:
- `postCreateCommand` runs `cargo fetch || true`
- `updateContentCommand` runs `cargo check --all-features || true`

This prevents transient network or registry issues from blocking container startup.
Run `cargo fetch` or `cargo check --all-features` manually if you want strict verification.

Cargo extension installs during image build are also best-effort. If a specific extension
tool is missing, install it manually inside the container with `cargo install --locked <tool>`.

### Slow Builds

- The mold linker is pre-configured for faster linking
- First build downloads dependencies; subsequent builds use the cached volumes
- If builds are still slow, check Docker resource allocation in Docker Desktop settings

### GPG Commit Signing

The `~/.gnupg` directory is mounted **read-only** for security. If you need commit signing:

**Option 1: GPG Agent Forwarding (Recommended)**
Configure your host's `gpg-agent` to forward to the container by adding to your host's `~/.gnupg/gpg-agent.conf`:
```
extra-socket /path/to/socket
```

**Option 2: SSH Signing**
Use SSH key signing instead of GPG:
```bash
git config --global gpg.format ssh
git config --global user.signingkey ~/.ssh/id_ed25519.pub
```

**Option 3: Modify the mount (Not Recommended)**
Edit `devcontainer.json` to remove `,readonly` from the `.gnupg` mount. This is less secure.

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
