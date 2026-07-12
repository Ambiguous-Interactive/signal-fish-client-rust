# Session 005 — Godot browser interoperability

## Outcome

Completed the local implementation and executable evidence for PR C. A pure
Rust Godot 4.5 GDExtension now builds as a no-thread Emscripten side module,
exports with the official template, and interoperates in Chromium with Signal
Fish Server 0.4.0.

## Completed

- Added an official Godot 4.5 browser workflow with checksum-verified editor
  and templates, pinned Rust/Emscripten/Playwright/server versions, custom
  32-bit Godot API generation, and failure-log artifacts.
- Expanded the fixture to independent JSON and MessagePack client pairs,
  application Ping/Pong, text relay, physical binary relay, graceful local
  close, and server-drain close-code 4000 attribution.
- Added a raw-Emscripten negative control. The same official template aborts
  on undefined `emscripten_websocket_new`, proving the optional JavaScript
  library is absent rather than relying on a documentation claim.
- Corrected Godot web build guidance for `api-custom`, Emscripten bindgen
  sysroot selection, side-module linking, immediate-abort panic behavior, and
  the pinned tier-3 Rust nightly.
- Disabled GDExtension reloadability after the official editor exposed an
  unload-time crash in native import validation.

## Verification

- Native headless fixture against server 0.4.0 passed all JSON, MessagePack,
  relay, shutdown, and drain markers.
- `wasm32-unknown-emscripten` release build completed without warnings using
  Godot 4.5 custom bindings and Emscripten 3.1.74.
- Official `web_dlink_nothreads_release` export completed and Chromium passed
  the full browser marker sequence.
- The negative-control browser run observed the expected unresolved
  `emscripten_websocket_new` abort and passed its rejection assertion.

## Remaining

- Run final repository-wide validation and commit the PR C browser milestone.
- Push/open the PR, await CI, and request all reviewers. GitHub CLI remains
  unauthenticated in this environment, so remote publication is still blocked.
