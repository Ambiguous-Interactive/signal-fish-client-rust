# Session 040 — Documentation Design System

## Priority and Hosted Audit

The session began from clean `main` at
`40a030bfd4a189f77d32641ebb7dbb8af17580aa`, the merge of PR #125. The GitHub
connector found no open or draft pull requests and no dependency pull
requests. All 12 push workflows on that commit completed successfully; Godot
Web run 32516835794 was the last to finish.

Issue #82's performance baselines, attribution, optimized hotspot, regression
ceilings, and owner-requested optimization were all complete through PRs #123
and #125. A criterion-by-criterion evidence comment was added and #82 was
closed as completed. Issue #90 remains a genuine administration blocker: live
ruleset 14801090 still lacks pull-request and required-status-check rules, and
the Repository Policy audit correctly fails. Restoring it requires repository
administration, quota handling, and an eligible independent reviewer.

With correctness, usability, and measured performance work complete, issue
#80 was the next actionable milestone. Its server-side prerequisite (#204) and
client prerequisite (#110) were already complete.

## Approved Source and Adaptation

The owner-supplied package is
`Signal.Fish.cloud.design.zip` from client issue #80:

- source: <https://github.com/user-attachments/files/30513171/Signal.Fish.cloud.design.zip>;
- size: 327,701 bytes;
- SHA-256: `e5568dd7ab3b337cb937f1f2c43e7334ba5b7f34f2b556a3f0ca43938772ffa1`.

It is byte-identical to the package used by the Signal Fish Server. The
cross-product decision in Server issue #204 approves the Vector mark, all
variants, an oceanic palette retune, and self-hosted fonts; server commit
`3d3944f89739d367ddb60f90fbea64352c834a28` is the implementation reference.
The archive has no separate artwork license. The recorded authority is the
Ambiguous Interactive owner's attached package and explicit direction in
issue #80, not a general trademark grant. The three bundled fonts retain their
complete SIL OFL 1.1 notices.

`docs/assets/PROVENANCE.toml` records the package, permission basis, server
reference, adaptation, and exact format, dimensions, color space, source,
license, byte size, and checksum for every other file recursively under
`docs/assets`; the manifest itself is the sole exception.

## Implementation

- Replaced the old repository banner with a client-specific, path-only SVG
  lockup built from the approved Vector mark and OFL font outlines. The README,
  MkDocs header, favicon, and home page now share that source-vector identity.
- Ported the approved oceanic system into the existing task-oriented Material
  site while correcting text, muted text, link, button, code-comment, and
  header tokens for AA contrast in both schemes.
- Self-hosted Space Grotesk, Hanken Grotesk, and JetBrains Mono. Material's
  runtime font provider is disabled, and the theme override preloads all three
  local subsets.
- Added clear home-page start actions, brand/font attribution, responsive
  table and banner behavior, visible `:focus-visible` treatment, reduced-motion
  handling, and print-safe colors without weakening the existing navigation,
  search, code-copy, admonition, or strict-build behavior.
- Added `docs_brand_policy` regression tests for exact asset coverage and
  checksums, design wiring, local-font delivery, accessible identity,
  intrinsic image dimensions, focus/reduced-motion/responsive rules, and WCAG
  contrast.

## Contrast Evidence

All normal-text combinations meet the 4.5:1 WCAG AA threshold; the Vector mark
also exceeds the 3:1 non-text threshold.

| Component | Light | Dark |
| --- | ---: | ---: |
| Body text / page | 17.62:1 | 17.01:1 |
| Secondary text / page | 6.27:1 | 7.97:1 |
| Lightest retained text / page | 4.87:1 | 5.45:1 |
| Link/accent / page | 4.95:1 | 12.36:1 |
| Primary button text / fill | 4.95:1 | 10.25:1 |
| Code comments / code surface | 4.94:1 | 5.95:1 |
| Header text / header | 16.68:1 | 16.30:1 |

The aqua Vector mark is 11.18:1 against its dark compact-logo surface.

## Browser and Visual Evidence

Playwright 1.61.1 with Chromium 149 exercised seven cold page states. Every
state loaded all three local fonts, had zero broken images, zero document-level
horizontal overflow, and cumulative layout shift 0. Screenshots are viewport
captures so the reviewed code, table, admonition, and navigation states remain
legible rather than being reduced into extremely tall full-page images.

| Evidence | Viewport | Scheme and coverage | SHA-256 |
| --- | --- | --- | --- |
| [Home](assets/session-040-design-system/desktop-home-dark.png) | 1440×1000 | Dark; identity, actions, admonition, tabs/TOC | `1b846e52e722dd7dad86eb9fd82de8b0b0940bd1278e8e9366f578074b3c3075` |
| [Home](assets/session-040-design-system/mobile-home-light.png) | 390×844 | Light; identity, actions, and admonition | `9301679a25f5c613b6cdcadd0b57ba9c5bfde82451ee4b78842acc1d5e760062` |
| [Home + drawer](assets/session-040-design-system/mobile-home-light-nav.png) | 390×844 | Light; logo and primary drawer open | `3fcf757c12d833c25969b957018434681edddd36992c7ae908ae12fc6750a4f9` |
| [Quick start](assets/session-040-design-system/desktop-quickstart-light.png) | 1440×1000 | Light; code and admonition | `f607b992e804ad00e14e853fe9c00d9a2e5fbf9dd3b32f7da83f611f8b89809a` |
| [Quick start](assets/session-040-design-system/mobile-quickstart-dark.png) | 390×844 | Dark; long code and admonition | `918b1ab8d1adc74dcba20e9d831319a44af8ac4348232fe7093b6d0ade859e30` |
| [Client API](assets/session-040-design-system/desktop-client-dark.png) | 1440×1000 | Dark; long reference, code, admonition, TOC | `044a1be57a114e3e851f74314f52ad18195e97188104a4f6d09645cb4e31daa5` |
| [Client API](assets/session-040-design-system/mobile-client-light.png) | 390×844 | Light; long reference table and code | `46840a7612ec0e2a404f5a8953bd70816a131f24d9df3f15cb05ec3ea8c1007d` |

An early mobile cold-load probe exposed a real 0.17 layout shift: the display
font sometimes arrived after the home heading first wrapped. Preloading all
three local fonts and giving both home images intrinsic dimensions eliminated
the race. Ten additional isolated cold Chromium launches produced the intended
single-line heading and cumulative layout shift 0 in every run.

Interaction checks also confirmed:

- keyboard Tab reaches the skip link with a 2px accent outline;
- the drawer, theme, and search controls expose named button semantics and
  activate with both Enter and Space while retaining a visible focus ring;
- opening the drawer transfers focus to its native close button, with the home
  link remaining a separate control and no nested interactive semantics;
- closing the drawer or search returns focus to its trigger, and closed mobile
  overlays are inert rather than leaving offscreen links in the Tab sequence;
- open drawer and search dialogs trap forward and reverse Tab navigation inside
  their modal surfaces, so obscured page actions cannot receive focus; Escape
  closes either surface and restores its trigger;
- collapsed TOC and section panels expose synchronized button, controls, and
  expanded state, remain inert, and never place off-canvas links in Tab order;
- opening a nested section focuses its visible Back control and scopes both Tab
  directions to that active subpanel until it is closed;
- desktop search returns the Client API reference for `SignalFishClient`;
- narrow-screen search opens its modal and returns the same reference for
  `JoinRoomParams`;
- the theme control changes `default` to `slate`;
- the narrow-screen primary drawer opens and remains fully visible;
- 390px → 1280px → 390px resizing and three instant-navigation transitions
  retain one correctly synchronized accessible shell.

## Verification and Review

Focused verification passed during implementation:

- all three `docs_brand_policy` tests;
- `mkdocs build --strict`;
- `scripts/check-docs-rendering.sh` (17/17 checks);
- seven-state Chromium rendering and interaction checks described above.

The frozen tree also passed:

- `scripts/ci-validate.sh`: 12 available phases passed, zero failed, with only
  the unavailable `typos`, `markdownlint`, and Docker checks skipped;
- `cargo fmt && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features`;
- `node --check docs/javascripts/accessibility.js`, `mkdocs build --strict`, and
  all 17 documentation rendering checks;
- `cargo deny check` and `cargo audit` with a freshly resolved local lockfile.

The final Chromium accessibility regression additionally cycled 30 forward and
30 reverse drawer Tab stops, 20 forward and 20 reverse search Tab stops with
live results, root and nested TOC panels, Escape/reopen behavior, and both
resize directions. Settled LTR, RTL, Home TOC, and nested Quick Start TOC
panels kept their complete focused bounds inside the 242px drawer.

Independent design/accessibility and code/contract adversarial reviewers each
reported zero remaining findings on the frozen tree. Hosted checks and review
state are intentionally recorded on the resulting pull request, where they can
be observed rather than predicted by this pre-publication session record.
