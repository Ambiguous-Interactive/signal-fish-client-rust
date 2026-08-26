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

## JSON allocation attribution

A same-lockfile Heaptrack comparison of `main` and the optimized head resolved
the 4 KiB outbound burst's original growth to two sites. Serde JSON's 128-byte
`to_string` buffer grew twice for each of 64 frames (128 reallocations), while
the polling command `VecDeque` grew four times. The 64-message binary control
has the same four queue reallocations and no serializer allocation. On the
optimized head, the Serde growth stacks disappear and only the four queue
growths remain.

Both drivers now share capacity-aware serialization only for direct JSON
string game payloads of at least 4 KiB, the measured class. The hint reads the
existing string length in constant time and leaves structured or smaller JSON
on Serde's default path. Serde remains the canonical encoder, including for
large escaped and multibyte strings; underestimated escaping merely uses its
normal safe buffer growth.

## Baseline

Timing was measured 2026-08-21 and allocation columns were refreshed
2026-08-22 on Rust 1.96.1, a 12-vCPU aarch64 WSL2 Linux runner. The timing
columns are release-profile medians over 25 samples pinned to one CPU.
Allocation columns are exact across 10 isolated samples in both debug and
release profiles; both profiles produced identical values. `A/D/R` means
allocation, deallocation, and reallocation. Byte columns use the same order.
Queue age is the maximum observed oldest queued frame age, so inbound-only
cells correctly report zero.

| Workload | Median ns | ns/op | ops/s | Peak queue ns | A/D/R | Bytes A/D/R |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `lobby/2` | 3301 | 3301 | 302938 | 0 | 17/5/0 | 3989/3752/0 |
| `lobby/8` | 5500 | 5500 | 181818 | 0 | 29/5/1 | 5053/4776/704 |
| `lobby/16` | 8801 | 8801 | 113623 | 0 | 49/7/2 | 8259/7304/2112 |
| `json/in/256/single` | 2500 | 2500 | 400000 | 0 | 3/3/0 | 2560/2992/0 |
| `json/in/256/burst64` | 34101 | 532 | 1876777 | 0 | 129/129/4 | 108544/136192/17280 |
| `json/in/4096/single` | 3000 | 3000 | 333333 | 0 | 3/3/0 | 6400/10672/0 |
| `json/in/4096/burst64` | 73204 | 1143 | 874269 | 0 | 133/133/8 | 355456/628864/13824 |
| `json/out/256/single` | 2000 | 2000 | 500000 | 700 | 1/1/2 | 582/256/454 |
| `json/out/256/burst64` | 54003 | 843 | 1185119 | 67104 | 64/64/132 | 47328/16384/39136 |
| `json/out/4096/single` | 3600 | 3600 | 277777 | 1000 | 1/1/0 | 4226/4096/0 |
| `json/out/4096/burst64` | 151207 | 2362 | 423260 | 119306 | 64/64/4 | 280544/262144/10080 |
| `binary/in/256/single` | 2200 | 2200 | 454545 | 0 | 3/3/0 | 2560/2944/0 |
| `binary/in/256/burst64` | 17201 | 268 | 3720713 | 0 | 129/129/4 | 108544/133120/17280 |
| `binary/in/4096/single` | 2300 | 2300 | 434782 | 0 | 3/3/0 | 6400/10624/0 |
| `binary/in/4096/burst64` | 30102 | 470 | 2126104 | 0 | 133/133/8 | 355456/625792/13824 |
| `binary/out/256/single` | 1700 | 1700 | 588235 | 900 | 0/0/0 | 0/0/0 |
| `binary/out/256/burst64` | 38902 | 607 | 1645159 | 49902 | 0/0/4 | 10080/0/10080 |
| `binary/out/4096/single` | 1700 | 1700 | 588235 | 900 | 0/0/0 | 0/0/0 |
| `binary/out/4096/burst64` | 42602 | 665 | 1502276 | 42002 | 0/0/4 | 10080/0/10080 |
| `classified/latest` | 59803 | 934 | 1070180 | 36602 | 64/256/132 | 50124/57600/41932 |
| `classified/volatile` | 58603 | 915 | 1092094 | 76204 | 64/256/132 | 50124/57600/41932 |
| `classified/authorized-gap` | 4100 | 4100 | 243902 | 0 | 14/11/0 | 7378/6472/0 |
| `reconnect/2` | 5101 | 5101 | 196039 | 700 | 25/10/1 | 6916/6534/128 |
| `reconnect/8` | 8001 | 8001 | 124984 | 600 | 37/10/3 | 8108/8582/960 |
| `reconnect/16` | 12301 | 12301 | 81294 | 801 | 61/16/5 | 12882/14470/2624 |
| `polling/ready-frame-burst` | 15601 | 917 | 1089673 | 61204 | 17/68/3 | 6880/11492/4704 |
| `polling/ready-byte-burst` | 6900 | 1725 | 579710 | 6100 | 4/16/8 | 6048/5376/5536 |
| `polling/pending-recovery` | 2600 | 2600 | 384615 | 1501 | 1/4/2 | 624/900/496 |

In alternating base/head timing runs pinned to the same CPU, the 4 KiB outbound
burst medians moved from 202,010/199,010 ns to 151,207/150,108 ns. Its checked
allocation record moved from `64/64/132` operations and
`537408/262144/529216` bytes to `64/64/4` and `279104/262144/8640`, while its
protocol-ledger digest and every ownership, backpressure, delivery, and
accountability invariant remained unchanged for that serialization change.

Negotiated room-operation identity subsequently enlarged the exhaustive
`ClientMessage` value carried in polling queue cells by 24 bytes. Queue growth
therefore moves 1,440 additional bytes in 64-command burst cells (672 bytes in
the 17-command ready-frame cell), without increasing allocation or
reallocation counts. All 28 protocol digests were intentionally refreshed
because v3 `Authenticate` bytes now request `room_operation_ids` and the event
ledger gained the `RoomOperationFailed` slot; every semantic ledger check still
passes.

The negotiated outbound message-size contract then added
`max_outbound_message_size` to the v3 `ProtocolInfo` fixture bytes, so all 28
protocol digests were refreshed again. The fixture is constructed outside
every measured region, so allocation and timing baselines are unchanged; all
semantic ledger invariants still pass.

The `JoinRoom` omission fix subsequently removed explicit `null` values for
unset optional members from every workload's setup wire. All affected protocol
digests were intentionally refreshed; reconnect-only cells without a join
remain unchanged. Fixture construction is still outside the measured regions,
all semantic ledger invariants pass, and both debug and release allocation
ceilings remain unchanged.
