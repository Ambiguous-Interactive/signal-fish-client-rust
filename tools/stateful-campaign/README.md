# Stateful Hostility Campaign

A deterministic, stateful randomized (model-based) hostility campaign against
the polling driver and the shared `ClientCore` (issue #219). Unlike the
stateless per-frame fuzz targets, this harness drives the **real client**
through long hostile-but-schema-valid server journeys and asserts on every
documented behavioral contract it can observe.

## What it does

- **Generator** — 12 scenario archetypes (journeys, roster storms,
  accountability stamp chaos, spectator churn, plan/generation churn, raw
  malformed frames, command storms, echo zoos, transport kills, and a
  send-pressure archetype) emitting all 32 `ServerMessage` variants and all
  9 `RoomOperationResult` faces with hostile fields: stale/replayed
  generations, overlapping and miscounted gap ranges, bound storms, and
  invalid client commands, across all four negotiation dialects and all
  three `ProtocolViolationPolicy` values.
- **Per-frame event-expectation oracle** — every delivered frame must produce
  exactly one documented outcome multiset (event, violation, documented
  silence). A silent swallow of a `Pong`/`Error`/`GameData` event, a
  fabricated event, or a double delivery is a finding. The oracle mirrors the
  client's published gates: negotiation and phase fences, room-operation
  fences with absorbed-late-reply tracking, relay-stats validity, and the
  Observe policy's diagnostic delivery.
- **Stats/ledger equivalence oracle** — `ClientStats` counters must equal the
  harness's independent count of decoded game-data frames, undecodable frames
  must surface exactly one `DecodeFailed` each, and accepted sends must reach
  the transport exactly once.
- **Send-pressure archetype** — a `Pending`-refusing `poll_send` face
  exercises outbound pacing: FIFO delivery, capacity accounting, and full
  drain under backpressure.
- **Round-41 oracle families** — phase-legality model, terminal discipline,
  snapshot coherence, positive quarantine latch, close-info attribution, and
  the cross-policy differential.

Findings reduce automatically to a minimal failing step prefix and reproduce
deterministically from `(seed, script index, policy)`.

## Usage

```shell
# Oracle canaries: every rejection branch is sensitivity-proven.
cargo run --locked --release -p signal-fish-client-stateful-campaign \
  --bin stateful-campaign -- --selftest

# Full campaign (the Deep Safety CI lane).
cargo run --locked --release -p signal-fish-client-stateful-campaign \
  --bin stateful-campaign -- --seeds 1500 --scripts 40 --budget-secs 900

# Reduced campaign, long-horizon churn probe, single-script replay.
cargo run --locked --release -p signal-fish-client-stateful-campaign \
  --bin stateful-campaign -- --seeds 24 --scripts 12
cargo run --locked --release -p signal-fish-client-stateful-campaign \
  --bin stateful-campaign -- --soak
cargo run --locked --release -p signal-fish-client-stateful-campaign \
  --bin stateful-campaign -- --repro 11 27 Q:61
```

`--repro SEED SCRIPT POLICY[:PREFIX]` replays one script verbosely
(`Q`/`O`/`D` pick the policy; `PREFIX` bounds the step count). Findings print
their own repro line. `STATEFUL_CAMPAIGN_BREAK_ORACLE=1` neuters the oracle's
rejection branches for sensitivity demonstrations.

## CI wiring

- Required nowhere (like fuzzing/mutation, too slow for every PR); the
  scheduled and PR-path-triggered **Deep Safety** workflow runs canaries plus
  the full campaign in the `Stateful hostility campaign` job.
- `scripts/check-all.sh` phase 23 mirrors both locally, fail-closed.
- The crate is a workspace member, so the mandatory fmt/clippy/test gates
  cover it; its unit tests run the canaries, a reduced smoke campaign, and
  the soak probe on every `cargo test --workspace`.

## Known oracle limitations

- The signal/plan family uses conservative alternatives (delivery or
  documented suppression) rather than a full generation-fence mirror, so a
  hypothetical defect that *swapped* one suppressed signal for another would
  not be caught. The in-repo protocol tests cover those faces exactly.
- The unsupported-format advisory tracking is an approximation of the
  client's armed-range bookkeeping; the causality face therefore keeps the
  violation alternatives legal even when this oracle believes the advisory is
  armed (never silent).
