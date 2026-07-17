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
