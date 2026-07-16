---
name: devcontainer-cross-platform
description: Keep the development container portable across host platforms. Use when editing devcontainer configuration, Docker mounts, credentials, post-start behavior, or compatibility validation.
---

# Devcontainer Cross-Platform Compatibility

## Core Reliability Rule

The committed `devcontainer.json` must be able to open in VS Code on Windows,
macOS, Linux, WSL, Codespaces, and remote Docker hosts without requiring
machine-specific host credential paths.

Do **not** commit required bind mounts for host-home secrets or dotfiles:

| Avoid | Why |
|-------|-----|
| `${localEnv:HOME}/.ssh` | `HOME` is not guaranteed on Windows |
| `${localEnv:HOME}/.gitconfig` | VS Code copies Git config automatically |
| `${localEnv:HOME}/.gnupg` | GPG layout differs by platform and user |
| `${localEnv:USERPROFILE}/...` | Windows-only and invalid on Unix |
| `~/.ssh`, `~/.gnupg`, `~/.gitconfig` | Shell expansion is not guaranteed in mount specs |

Required bind mounts fail before the container starts if the source path is
missing, not shared with Docker Desktop, or expanded from an unset environment
variable. Keep personal credential mounts in local, uncommitted overrides.

## Git Credential Pattern

Rely on VS Code Dev Containers' built-in credential handling:

- Git config is copied into the container by VS Code.
- HTTPS credentials are reused through the host credential helper.
- SSH remotes work through the forwarded host SSH agent when it is running.
- Commit signing should use SSH signing or VS Code/GPG agent integration, not a
  committed `~/.gnupg` bind mount.

This repository's committed `mounts` should stay limited to Docker named volumes
for caches unless there is a hard, cross-platform project requirement.

## Lifecycle Commands

The `initializeCommand` in `devcontainer.json` runs on the **host machine**, not
inside the container. It may run during initial creation and later starts.

| Form | Behavior | Platform notes |
|------|----------|----------------|
| `string` | Runs through the host shell | Must be valid for host shells |
| `array` | Executes directly, no shell parsing | First executable must exist on every host |
| `object` | Named commands run in parallel | Each value must be cross-platform |

Avoid Unix-only syntax in host-side lifecycle commands:

| Pitfall | Explanation |
|---------|-------------|
| `mkdir -p` | Fails in Windows `cmd.exe` |
| `touch file` | Not available in Windows `cmd.exe` |
| `2>/dev/null` | Windows uses `2>nul` |
| `\|\| true` | `true` is not a `cmd.exe` builtin |
| `["bash", "-c", "..."]` | `bash` is not guaranteed on Windows hosts |

If a future host initialization step is truly unavoidable, prefer a
cross-platform executable already required by the project, or use a documented
PowerShell/Unix fallback. The command must be idempotent because
`initializeCommand` can run more than once.

## Automated Enforcement

`scripts/check-devcontainer-compat.sh` enforces these rules. It:

1. Parses `devcontainer.json` as JSONC.
2. Validates `initializeCommand` string, array, and object forms.
3. Rejects Unix-only host command patterns without a Windows-compatible path.
4. Rejects required bind mounts from host-home credential paths.
5. Requires referenced devcontainer host-initializer scripts only when the
   corresponding command references them.

`docker buildx build --check -f .devcontainer/Dockerfile .` statically checks
Dockerfile parse/build issues without running a full image build. Keep it in CI
when the Dockerfile uses BuildKit-specific syntax such as cache mounts.

The check is integrated into:

- `.github/workflows/workflow-lint.yml`
- `scripts/ci-validate.sh`
- `scripts/install-hooks.sh`

Unit tests live in `scripts/test_check_devcontainer_compat.sh`. When changing
devcontainer policy, add a failing fixture first, then update the check and the
real config.

## Current Repository Policy

The committed devcontainer has no `initializeCommand` because it has no required
host prerequisites. Its mount list is limited to named volumes:

- Cargo registry cache
- Cargo git cache
- `target/` build cache

This is intentional. Do not reintroduce committed `~/.ssh`, `~/.gitconfig`, or
`~/.gnupg` bind mounts to make a single workstation easier at the expense of
cross-platform startup reliability.
