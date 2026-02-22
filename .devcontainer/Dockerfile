# syntax=docker/dockerfile:1
# =============================================================================
# Signal Fish Client SDK - Development Container
# =============================================================================
# Production-ready Rust development environment with comprehensive tooling.
# Base: Official Microsoft Rust devcontainer image (Debian Bookworm)
#
# Build-time performance optimizations applied:
#  1. docker-clean removed — preserves .deb files in BuildKit cache mounts.
#  2. APT cache mounts     — .deb files and package index survive across
#                            rebuilds, saving 2-5 minutes per build.
#  3. Two-layer APT        — base packages cached independently from external
#                            repos for maximum cache hit rate.
#  4. cargo-binstall       — installs Rust tools as prebuilt binaries instead
#                            of compiling from source, saving 10-15 min/build.
#  5. Layer ordering       — stable layers first; volatile layers last.
# =============================================================================

# For hermetic/reproducible builds, pin to a specific image digest:
# FROM mcr.microsoft.com/devcontainers/rust:1-bookworm@sha256:<digest>
# Find digests at: https://mcr.microsoft.com/v2/devcontainers/rust/tags/list
FROM mcr.microsoft.com/devcontainers/rust:1-bookworm

# =============================================================================
# OCI Labels (https://github.com/opencontainers/image-spec/blob/main/annotations.md)
# =============================================================================
LABEL org.opencontainers.image.title="Signal Fish Client SDK Dev Container" \
      org.opencontainers.image.description="Rust development environment for Signal Fish Client SDK" \
      org.opencontainers.image.vendor="Ambiguous Interactive" \
      org.opencontainers.image.source="https://github.com/ambiguous-interactive/signal-fish-client" \
      org.opencontainers.image.licenses="MIT"

# Use bash with pipefail for consistent behavior
SHELL ["/bin/bash", "-o", "pipefail", "-c"]

# Prevent interactive prompts during package installation
ENV DEBIAN_FRONTEND=noninteractive

# =============================================================================
# Remove docker-clean hook + configure APT defaults
# =============================================================================
# The Microsoft devcontainer base image ships /etc/apt/apt.conf.d/docker-clean,
# which instructs apt to delete .deb files and package lists after every install.
# This defeats BuildKit's --mount=type=cache for /var/cache/apt. Removing it
# and enabling package retention allows cache mounts below to persist downloaded
# .deb files across rebuilds, saving 2-5 minutes.
#
# APT retry/timeout options are set globally here so all subsequent apt-get
# update and apt-get install calls inherit them automatically.
#
# Note: cache mount contents are NOT baked into the image layer — they live only
# in BuildKit's cache store — so image size is unaffected.
RUN rm -f /etc/apt/apt.conf.d/docker-clean \
    && printf '%s\n' \
        'APT::Keep-Downloaded-Packages "true";' \
        'Acquire::Retries "5";' \
        'Acquire::http::Timeout "30";' \
        'Acquire::https::Timeout "30";' \
        > /etc/apt/apt.conf.d/devcontainer

# =============================================================================
# Layer 1: Base System Packages
# =============================================================================
# BuildKit cache mounts keep downloaded .deb files and the package index alive
# across builds. Data in cache mounts is NOT committed to the image layer.
#
# Note: do NOT add `rm -rf /var/lib/apt/lists/*` here — that would wipe the
# cache mount, defeating the caching. The mount contents are not in the image.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update \
    && apt-get install -y --no-install-recommends \
        # Core utilities
        curl \
        wget \
        gnupg \
        ca-certificates \
        # Build dependencies for cargo tools
        pkg-config \
        libssl-dev \
        # Development tools
        ripgrep \
        fd-find \
        bat \
        jq \
        fzf \
        # Fast linker (clang required as linker driver for mold)
        # Note: mold and clang are unpinned to receive security updates.
        # Debian's exact versions change frequently and vary by architecture.
        # The base image pin (above) provides reproducibility when needed.
        mold \
        clang

# =============================================================================
# Layer 2: External Repository Packages (eza, GitHub CLI)
# =============================================================================
# Separated from Layer 1 so that base packages remain cached even when external
# repo GPG keys or URLs change. The apt cache mounts are shared across layers.
#
# The entire external-repo setup and installation is wrapped in a single
# conditional so that any network failure — including GPG key fetches — causes
# a graceful WARNING rather than a hard build failure.
#
# Note: GPG keys are verified via apt's signed-by mechanism during package
# installation. We don't pin key checksums because upstream keys may rotate;
# the GPG signature verification provides sufficient supply chain security for
# this dev environment.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    if mkdir -p /etc/apt/keyrings \
        # Set up eza repository with GPG key verification
        && wget -qO /tmp/eza.asc https://raw.githubusercontent.com/eza-community/eza/main/deb.asc \
        && gpg --dearmor < /tmp/eza.asc > /etc/apt/keyrings/gierens.gpg \
        && rm /tmp/eza.asc \
        && echo "deb [signed-by=/etc/apt/keyrings/gierens.gpg] https://deb.gierens.de stable main" \
            > /etc/apt/sources.list.d/gierens.list \
        && chmod 644 /etc/apt/keyrings/gierens.gpg /etc/apt/sources.list.d/gierens.list \
        # Set up GitHub CLI repository
        && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
            -o /usr/share/keyrings/githubcli-archive-keyring.gpg \
        && chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg \
        && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
            > /etc/apt/sources.list.d/github-cli.list \
        # Update index for new repos and install
        && apt-get update \
        && apt-get install -y --no-install-recommends eza gh; then \
        eza --version || true; \
        gh --version || true; \
    else \
        echo "WARNING: optional tools (eza/gh) unavailable; continuing build"; \
    fi

# =============================================================================
# Layer 3: git-delta (Better git diffs) - pinned version for reproducibility
# =============================================================================

# Note: Delta is downloaded from GitHub releases without checksum verification.
# This is a trade-off for maintainability - checksums would need updating with each
# version bump and differ per architecture. For production/sensitive environments,
# consider building from source or adding checksum verification.
# Build this container only from trusted networks.
ARG DELTA_VERSION=0.18.2
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    ARCH=$(dpkg --print-architecture) \
    && if curl -fsSL --retry 3 --retry-delay 2 \
            "https://github.com/dandavison/delta/releases/download/${DELTA_VERSION}/git-delta_${DELTA_VERSION}_${ARCH}.deb" \
            -o /tmp/delta.deb; then \
         # Use apt-get (not dpkg -i) to automatically resolve runtime library dependencies
         if apt-get update && apt-get install -y --no-install-recommends /tmp/delta.deb; then \
             delta --version || true; \
         else \
             echo "WARNING: delta package install failed; continuing without delta"; \
         fi; \
     else \
         echo "WARNING: delta download failed; continuing without delta"; \
     fi \
    && rm -f /tmp/delta.deb

# =============================================================================
# Git Configuration (system-level, as root)
# =============================================================================
# Written to /etc/gitconfig (via --system) so it survives the read-only
# ~/.gitconfig bind mount defined in devcontainer.json. Writing to --global
# (i.e., ~/.gitconfig) during build would be pointless: devcontainer.json
# completely replaces that file with the host's ~/.gitconfig at container start.
# Note: user-level keys in ~/.gitconfig still take precedence over these system
# defaults — users can override any setting in their own git config.
RUN if command -v delta >/dev/null 2>&1; then \
        git config --system core.pager "delta" \
        && git config --system interactive.diffFilter "delta --color-only" \
        && git config --system delta.navigate true \
        && git config --system delta.light false \
        && git config --system delta.line-numbers true \
        && git config --system delta.side-by-side true; \
    else \
        echo "INFO: delta not found; leaving git pager default"; \
    fi \
    && git config --system merge.conflictstyle diff3 \
    && git config --system diff.colorMoved default

# =============================================================================
# Cargo Extensions (as vscode user)
# =============================================================================

USER vscode

# Ensure ~/.cargo/bin is in PATH for all subsequent RUN commands.
# Non-login, non-interactive bash (used by Docker RUN) does not source .bashrc
# or .profile, so PATH must be set via ENV rather than relying on profile scripts.
ENV PATH="/home/vscode/.cargo/bin:${PATH}"

# =============================================================================
# Cargo Configuration (mold linker)
# =============================================================================

# Copy cargo config early so mold is available for any source-compilation
# fallbacks during tool installation (cargo-binstall falls back to cargo install
# when no prebuilt binary exists for a crate).
COPY --chown=vscode:vscode .devcontainer/.cargo/config.toml /home/vscode/.cargo/config.toml

# =============================================================================
# Layer 4: MSRV Verification and Rust Components
# =============================================================================

# Verify Rust version meets minimum supported version (MSRV: 1.85.0), then
# add rust-src for std library browsing. Combined in one layer to avoid an
# extra image layer for a trivial command.
RUN RUST_VERSION=$(rustc --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+') \
    && REQUIRED_VERSION="1.85.0" \
    && if [ "$(printf '%s\n' "$REQUIRED_VERSION" "$RUST_VERSION" | sort -V | head -n1)" != "$REQUIRED_VERSION" ]; then \
         echo "ERROR: Rust $RUST_VERSION does not meet MSRV $REQUIRED_VERSION" && exit 1; \
       fi \
    && echo "Rust $RUST_VERSION meets MSRV $REQUIRED_VERSION" \
    && rustup component add rust-src

# =============================================================================
# Layer 5: cargo-binstall (bootstrapped from prebuilt binary)
# =============================================================================
# cargo-binstall installs Rust tools as prebuilt binaries instead of compiling
# them from source. This is the key optimization: installing 10 tools drops
# from ~10-15 minutes (source compilation) to ~1-2 minutes (binary downloads).
#
# cargo-binstall itself is bootstrapped by downloading its prebuilt binary
# directly from GitHub releases — no Rust compilation required.
#
# Pinned to a specific version for reproducibility. Update by bumping
# CARGO_BINSTALL_VERSION below. Available versions:
#   https://github.com/cargo-bins/cargo-binstall/releases
ARG CARGO_BINSTALL_VERSION=1.17.5
RUN tmpdir=$(mktemp -d) \
    && ARCH=$(uname -m) \
    && case "$ARCH" in \
        x86_64)  BINSTALL_ARCH="x86_64-unknown-linux-musl" ;; \
        aarch64) BINSTALL_ARCH="aarch64-unknown-linux-musl" ;; \
        *) echo "ERROR: Unsupported architecture for cargo-binstall: $ARCH" && exit 1 ;; \
    esac \
    && curl -fsSL --retry 3 --retry-delay 2 \
        "https://github.com/cargo-bins/cargo-binstall/releases/download/v${CARGO_BINSTALL_VERSION}/cargo-binstall-${BINSTALL_ARCH}.tgz" \
        -o "$tmpdir/cargo-binstall.tgz" \
    && tar -xzf "$tmpdir/cargo-binstall.tgz" -C "$tmpdir" \
    # Use -print -quit to avoid a broken pipe with pipefail when piping to head -n1
    && BINSTALL_BIN=$(find "$tmpdir" -name "cargo-binstall" -type f -print -quit) \
    && [ -n "$BINSTALL_BIN" ] || { echo "ERROR: cargo-binstall binary not found in extracted tarball"; exit 1; } \
    && mkdir -p ~/.cargo/bin \
    && mv "$BINSTALL_BIN" ~/.cargo/bin/cargo-binstall \
    && chmod +x ~/.cargo/bin/cargo-binstall \
    && rm -rf "$tmpdir" \
    # -V prints cargo-binstall's own version; --version <VER> means "install this crate version"
    && cargo-binstall -V

# =============================================================================
# Layer 6: Cargo Tools via cargo-binstall (prebuilt binaries)
# =============================================================================
# cargo-binstall fetches prebuilt binaries from each crate's GitHub releases
# and falls back to source compilation only when no binary is available.
# The Cargo registry cache mount preserves downloaded crate sources across
# rebuilds, making any source-compiled fallbacks faster on subsequent builds.
#
# Versions pinned for reproducibility — update periodically.
RUN --mount=type=cache,target=/home/vscode/.cargo/registry,uid=1000,gid=1000 \
    --mount=type=cache,target=/home/vscode/.cargo/git,uid=1000,gid=1000 \
    tools=( \
      cargo-watch@8.5.2 \
      cargo-edit@0.13.0 \
      cargo-audit@0.21.0 \
      cargo-deny@0.16.1 \
      cargo-outdated@0.15.0 \
      cargo-expand@1.0.88 \
      cargo-bloat@0.12.1 \
      cargo-nextest@0.9.82 \
      cargo-tarpaulin@0.31.3 \
      cargo-machete@0.7.0 \
    ) \
    && failed_tools=() \
    && for tool in "${tools[@]}"; do \
         echo "Installing optional cargo tool: ${tool}"; \
         if cargo binstall --no-confirm "${tool}"; then \
             echo "Installed ${tool}"; \
         else \
             echo "WARNING: Failed to install optional cargo tool '${tool}'; continuing build"; \
             failed_tools+=("${tool}"); \
         fi; \
       done \
    && if [ ${#failed_tools[@]} -gt 0 ]; then \
         echo "WARNING: Optional cargo tools not installed: ${failed_tools[*]}"; \
       fi

# =============================================================================
# Shell Aliases
# =============================================================================

# printf '%s\n' writes each argument as a separate line, producing correct
# newlines in .bashrc without heredoc (which confuses older Docker linters)
# or the literal-\n pitfall of `echo '\n\...'` in single-quoted strings.
RUN printf '%s\n' \
    '' \
    '# ============================================================' \
    '# Cargo Aliases' \
    '# ============================================================' \
    'alias ct="cargo test"' \
    'alias cb="cargo build"' \
    'alias cr="cargo run"' \
    'alias cc="cargo check"' \
    'alias cf="cargo fmt"' \
    'alias cl="cargo clippy"' \
    'alias cw="cargo watch"' \
    'alias cwt="cargo watch -x test"' \
    'alias cwc="cargo watch -x check"' \
    'alias cwr="cargo watch -x run"' \
    'alias cn="cargo nextest run"' \
    '' \
    '# Cargo with all features' \
    'alias cta="cargo test --all-features"' \
    'alias cba="cargo build --all-features"' \
    'alias cca="cargo check --all-features"' \
    'alias cla="cargo clippy --all-targets --all-features -- -D warnings"' \
    '' \
    '# Cargo tools' \
    'alias cdoc="cargo doc --no-deps --open"' \
    'alias cupdate="cargo update"' \
    'alias caudit="cargo audit"' \
    'alias cdeny="cargo deny check"' \
    'alias coutdated="cargo outdated"' \
    'alias cbloat="cargo bloat --release"' \
    'alias cexpand="cargo expand"' \
    'alias ctarp="cargo tarpaulin --all-features"' \
    '' \
    '# Full check (matches CLAUDE.md workflow)' \
    'alias ccheck-all="cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features"' \
    '' \
    '# ============================================================' \
    '# CLI Tool Aliases' \
    '# ============================================================' \
    'if command -v eza >/dev/null 2>&1; then' \
    '    alias ls="eza --icons"' \
    '    alias ll="eza -la --icons"' \
    '    alias la="eza -la --icons"' \
    '    alias lt="eza --tree --icons"' \
    'else' \
    '    alias ls="ls --color=auto"' \
    '    alias ll="ls -alF --color=auto"' \
    '    alias la="ls -A --color=auto"' \
    '    alias lt="find . -maxdepth 2 -print"' \
    'fi' \
    'alias bcat="batcat --style=plain"' \
    'alias bat="batcat"' \
    'alias fd="fdfind"' \
    'alias fdf="fdfind"' \
    'alias rg="rg --smart-case"' \
    '' \
    '# ============================================================' \
    '# Git Aliases' \
    '# ============================================================' \
    'alias gs="git status"' \
    'alias gd="git diff"' \
    'alias gl="git log --oneline -20"' \
    'alias gp="git pull"' \
    'alias gc="git commit"' \
    'alias ga="git add"' \
    >> ~/.bashrc

WORKDIR /workspaces
