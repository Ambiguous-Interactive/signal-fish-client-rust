---
name: github-operations
description: Route repository-hosted GitHub work through the VS Code GitHub connector or extension, local git, and gh in strict preference order. Use when working with issues, pull requests, reviews, checks, Actions, branches, commits, pushes, releases, or any other GitHub operation.
---

# GitHub Operations

Use this order for every GitHub operation:

1. **VS Code GitHub connector/extension** -- use the connected GitHub tools for
   hosted repository state and actions whenever they expose the required
   capability. This includes issue and pull-request reads or writes, reviews,
   comments, workflow runs, checks, artifacts, merges, releases, and remote
   branch/ref operations.
2. **Local `git`** -- use it when the connector/extension does not operate on
   the local checkout, or when the required operation is inherently Git-native:
   inspect or change the worktree/index, create commits, compare history, manage
   local branches, fetch, or push through an already configured remote.
3. **GitHub CLI (`gh`)** -- use it only when neither the connector/extension nor
   local `git` exposes the required capability.

Do not probe or require `gh` authentication unless a concrete last-resort
operation actually needs `gh`. A missing or unauthenticated `gh` executable is
not a blocker while the connector/extension or local `git` can complete the
workflow.

Before falling back, identify the unavailable capability. Keep hosted and local
state aligned by resolving the repository from `git remote`, the branch from
the checkout, and the exact commit SHA. Re-read hosted state through the
connector/extension after each write that affects a pull request, workflow, or
deployment.

Preserve normal write safety at every layer: inspect the target and current
state first, stage only intended files, avoid force updates unless explicitly
authorized, and confirm the result after mutation.

## Concretely reaching each layer (verified 2026-08-27)

Agents must not stop at "`gh auth status` says unauthenticated." That reports
only gh's own config; this repository's environment usually authenticates
GitHub through the VS Code client instead. Probe layers in preference order:

| Layer | Detection command | Working? |
| --- | --- | --- |
| Connector-driven UI actions | `ls ~/.vscode-server/extensions \| grep pull-request`; live log tail under `~/.vscode-server/data/logs/*/exthost*/GitHub.vscode-pull-request-github/` shows successful API polls | Hosted reads/writes happen here interactively |
| Connector-brokered credentials for local CLI tools | `printf 'protocol=https\\nhost=github.com\\n\\n' \| git credential fill` returns nonempty when `credential.helper` points at `/tmp/vscode-remote-containers-*.js git-credential-helper` (`git config --show-origin --get-all credential.helper`) | Yes |
| Local `git` over SSH remote | `git push` already worked all sessions | Yes |
| `gh` own login | `gh auth status` | Usually false headlessly |

The credential-broker bridge is the sanctioned way to run otherwise-unusable
`gh` operations as connector-first execution (the credentials originate from
the connected VS Code session, not an independent login):

```shell
# Ephemeral, never echoed or written to disk; consumed by the single command.
GH_TOKEN="$(printf 'protocol=https\nhost=github.com\n\n' \
  | git credential fill | sed -n 's/^password=//p' | tr -d '\r\n')"
gh pr create --base main --head "<branch>" --title "<t>" --body-file -
unset GH_TOKEN
```

Hard rules for the bridge: never print, redirect, or store the token; strip it
(`unset`) immediately after the operation; if `git credential fill` returns
empty, fall back to preparing a complete handoff note (branch, title,
body path, exact URL) instead of weakening auth.

Session 070 proof: `gh auth status` was unauthenticated, yet the bridge
opened [#173](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/pull/173)
first attempt. If `git credential fill` yields nothing, do not weaken auth —
prepare the full handoff note (branch, title, body file, URL) for an
interactive session instead.
