# ClientCore performance laboratory

This unpublished workspace package measures the public polling client, which
drives the same `ClientCore` used by the async client. Its 28 deterministic
cells cover lobby snapshots, JSON and binary relay in both directions,
reliable/latest/volatile delivery, an authorized sequence gap, reconnect
replay, ready-send frame and byte budgets, receive-byte budgets, and a
pre-acceptance `Pending` recovery.

Run the exact-accountability smoke suite and the two measurement modes with:

```shell
cargo run -p signal-fish-client-perf-lab --features perf --bin perf-smoke
cargo run --release -p signal-fish-client-perf-lab --features perf --bin perf-timing
cargo run -p signal-fish-client-perf-lab --features perf --bin perf-allocations
cargo run --release -p signal-fish-client-perf-lab --features perf --bin perf-allocations
```

After an intentional reviewed fixture or ledger change, generate candidate
protocol pins for diffing with:

```shell
cargo run -p signal-fish-client-perf-lab --features perf --bin perf-smoke -- --emit-protocol-baselines
```

That mode still enforces every semantic ledger invariant; only the checked-in
digest comparison is skipped so the proposed replacement is observable.

Fixture construction, serialization, warm-up, digesting, and ledger
verification are outside each measured region. Timing defaults to 25 samples
after one sacrificial run. Allocation accounting uses an isolated child
process for each of 10 samples and records allocation, deallocation, and
reallocation counts and bytes. Planted allocate/deallocate/reallocate controls
fail closed if the allocator is disconnected. Every sample must reproduce the
same protocol ledger before it can be reported.

The boundary is intentionally client-local: it excludes Tokio scheduling,
real sockets and TLS, server work, Godot, browser behavior, and Fortress codec
or rollback work. Those layers retain their existing system and real-server
evidence; these numbers describe polling-driver and shared-core work only.

CI runs on pinned Ubuntu 24.04 ARM64 and Rust 1.96.1. It treats timing as
diagnostic, but gates the full deterministic ledger against
`protocol-baselines.json` and all six allocation counters against
`allocation-baselines.json`. Nonzero ceilings are the observed baseline plus
10%, with a minimum margin of two operations or 256 bytes. The two single
binary outbound cells are deliberate zero-allocation contracts and therefore
have exact zero ceilings. Update a ceiling only with a reviewed explanation of
the measured implementation change. `stats_alloc` is exact-pinned; compatible
SDK serialization dependency updates remain inside the contract so an
allocation regression is reviewed instead of silently hidden by a lockfile.

## Baseline

Captured 2026-08-21 on Rust 1.96.1, a 12-vCPU aarch64 WSL2 Linux runner. The
timing columns are release-profile medians over 25 samples. Allocation columns
are exact across 10 isolated samples in both debug and release profiles; both
profiles produced identical values. `A/D/R` means allocation, deallocation,
and reallocation. Byte columns use the same order. Queue age is the maximum
observed oldest queued frame age, so inbound-only cells correctly report zero.

| Workload | Median ns | ns/op | ops/s | Peak queue ns | A/D/R | Bytes A/D/R |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `lobby/2` | 3000 | 3000 | 333333 | 0 | 17/5/0 | 3989/3752/0 |
| `lobby/8` | 5000 | 5000 | 200000 | 0 | 29/5/1 | 5053/4776/704 |
| `lobby/16` | 7800 | 7800 | 128205 | 0 | 49/7/2 | 8259/7304/2112 |
| `json/in/256/single` | 2001 | 2001 | 499750 | 0 | 3/3/0 | 2560/2992/0 |
| `json/in/256/burst64` | 31802 | 496 | 2012452 | 0 | 129/129/4 | 108544/136192/17280 |
| `json/in/4096/single` | 2600 | 2600 | 384615 | 0 | 3/3/0 | 6400/10672/0 |
| `json/in/4096/burst64` | 70704 | 1104 | 905182 | 0 | 133/133/8 | 355456/628864/13824 |
| `json/out/256/single` | 1900 | 1900 | 526315 | 600 | 1/1/2 | 582/256/454 |
| `json/out/256/burst64` | 50303 | 785 | 1272289 | 46503 | 64/64/132 | 45888/16384/37696 |
| `json/out/4096/single` | 3100 | 3100 | 322580 | 800 | 1/1/2 | 8262/4096/8134 |
| `json/out/4096/burst64` | 182510 | 2851 | 350665 | 298317 | 64/64/132 | 537408/262144/529216 |
| `binary/in/256/single` | 1800 | 1800 | 555555 | 0 | 3/3/0 | 2560/2944/0 |
| `binary/in/256/burst64` | 15901 | 248 | 4024904 | 0 | 129/129/4 | 108544/133120/17280 |
| `binary/in/4096/single` | 1900 | 1900 | 526315 | 0 | 3/3/0 | 6400/10624/0 |
| `binary/in/4096/burst64` | 27401 | 428 | 2335681 | 0 | 133/133/8 | 355456/625792/13824 |
| `binary/out/256/single` | 1500 | 1500 | 666666 | 800 | 0/0/0 | 0/0/0 |
| `binary/out/256/burst64` | 35102 | 548 | 1823257 | 47003 | 0/0/4 | 8640/0/8640 |
| `binary/out/4096/single` | 1500 | 1500 | 666666 | 600 | 0/0/0 | 0/0/0 |
| `binary/out/4096/burst64` | 38102 | 595 | 1679701 | 48203 | 0/0/4 | 8640/0/8640 |
| `classified/latest` | 55403 | 865 | 1155172 | 45302 | 64/256/132 | 48684/57600/40492 |
| `classified/volatile` | 55003 | 859 | 1163572 | 43202 | 64/256/132 | 48684/57600/40492 |
| `classified/authorized-gap` | 3600 | 3600 | 277777 | 0 | 14/11/0 | 7378/6472/0 |
| `reconnect/2` | 4601 | 4601 | 217344 | 600 | 25/10/1 | 6916/6534/128 |
| `reconnect/8` | 7500 | 7500 | 133333 | 600 | 37/10/3 | 8108/8582/960 |
| `reconnect/16` | 11301 | 11301 | 88487 | 601 | 61/16/5 | 12882/14470/2624 |
| `polling/ready-frame-burst` | 13901 | 817 | 1222933 | 9401 | 17/68/3 | 6208/11492/4032 |
| `polling/ready-byte-burst` | 6301 | 1575 | 634819 | 4101 | 4/16/8 | 6048/5376/5536 |
| `polling/pending-recovery` | 2300 | 2300 | 434782 | 1500 | 1/4/2 | 624/900/496 |

The data identifies JSON serialization, especially 4 KiB outbound bursts, as
the current time and byte-allocation hotspot. That is evidence for a future
optimization, not permission to weaken frame ownership, backpressure, event
delivery, or exact accountability.
