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

Ledger digesting is payload-complete: every payload-carrying event kind must
have a reviewed `fingerprint_event_payload` arm (optional `seq`/`epoch`/`key`
faces fingerprint a presence byte first, so `None` and `Some(0)` stay
distinguishable), and a workload that emits an arm-less kind fails loudly
instead of digesting as a bare count. Extending the ledger with a new event
kind therefore forces a reviewed arm and a digest refresh in the same change.

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
10%, with a minimum margin of two operations or 256 bytes; a zero operation
count carries a ceiling of one so any first allocation is caught. The two
single binary outbound cells are deliberate zero-allocation contracts and
have exact zero ceilings. Update a ceiling only with a reviewed explanation of
the measured implementation change. `stats_alloc` is exact-pinned; compatible
SDK serialization dependency updates remain inside the contract so an
allocation regression is reviewed instead of silently hidden by a lockfile.
The `toolchain` field in the allocation baseline JSON pins the compiler the
ceilings were recorded on, and the allocation harness refuses a record whose
field disagrees with its expectation so any intentional change is reviewed;
the running compiler itself is enforced separately by CI's pinned-toolchain
step.

## JSON allocation attribution

A same-lockfile Heaptrack comparison of `main` and the optimized head resolved
the 4 KiB outbound burst's original growth to two sites. Serde JSON's 128-byte
`to_string` buffer grew twice for each of 64 frames (128 reallocations), while
the polling command `VecDeque` grew four times. The 64-message binary control
has the same four queue reallocations and no serializer allocation. On the
optimized head, the Serde growth stacks disappear and only the four queue
growths remain.

Both drivers share capacity-aware serialization for every direct JSON
string game payload. The hint reads the existing string length in constant
time and leaves structured JSON on Serde's default path. Serde remains the
canonical encoder, including for large escaped and multibyte strings;
underestimated escaping merely uses its normal safe buffer growth. Inbound
binary frames likewise reuse the transport's wire buffer for the payload —
the strict decoder locates the payload in place and the driver-owned frame
buffer becomes the event's payload allocation, removing one payload copy
per binary frame in both drivers.

## Baseline

Timing was measured 2026-08-21 and allocation columns were refreshed
2026-08-22 on Rust 1.96.1, a 12-vCPU aarch64 WSL2 Linux runner. The timing
columns are release-profile medians over 25 samples pinned to one CPU.
Allocation columns are exact across 10 isolated samples in both debug and
release profiles; both profiles produced identical values. `A/D/R` means
allocation, deallocation, and reallocation. Byte columns use the same order.
Queue age is the maximum observed oldest queued frame age, so inbound-only
cells correctly report zero. The six rows changed by the 2026-09-11
capacity-hint extension and zero-copy inbound binary path were re-measured
on Rust 1.98.0; A/D/R counts are exact across toolchains, and byte columns
shift a few hundred bytes with the toolchain's allocator bins. Those six
ceilings were tightened to the new observed values plus the standard margin
after the counts were verified exact across stable-debug, stable-release,
and nightly-release toolchains; the baseline file's `toolchain` field stays
the CI verification toolchain (1.96.1).

| Workload | Median ns | ns/op | ops/s | Peak queue ns | A/D/R | Bytes A/D/R |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `lobby/2` | 3301 | 3301 | 302938 | 0 | 17/5/0 | 3989/3752/0 |
| `lobby/8` | 5500 | 5500 | 181818 | 0 | 29/5/1 | 5053/4776/704 |
| `lobby/16` | 8801 | 8801 | 113623 | 0 | 49/7/2 | 8259/7304/2112 |
| `json/in/256/single` | 2500 | 2500 | 400000 | 0 | 3/3/0 | 2560/2992/0 |
| `json/in/256/burst64` | 34101 | 532 | 1876777 | 0 | 129/129/4 | 108544/136192/17280 |
| `json/in/4096/single` | 3000 | 3000 | 333333 | 0 | 3/3/0 | 6400/10672/0 |
| `json/in/4096/burst64` | 73204 | 1143 | 874269 | 0 | 133/133/8 | 355456/628864/13824 |
| `json/out/256/single` | 2000 | 2000 | 500000 | 700 | 1/1/0 | 386/256/0 |
| `json/out/256/burst64` | 54003 | 843 | 1185119 | 67104 | 64/64/4 | 36224/16384/11520 |
| `json/out/4096/single` | 3600 | 3600 | 277777 | 1000 | 1/1/0 | 4226/4096/0 |
| `json/out/4096/burst64` | 151207 | 2362 | 423260 | 119306 | 64/64/4 | 281984/262144/11520 |
| `binary/in/256/single` | 2200 | 2200 | 454545 | 0 | 2/2/0 | 2368/2368/0 |
| `binary/in/256/burst64` | 17201 | 268 | 3720713 | 0 | 65/65/4 | 94720/94720/17760 |
| `binary/in/4096/single` | 2300 | 2300 | 434782 | 0 | 2/2/0 | 2368/2368/0 |
| `binary/in/4096/burst64` | 30102 | 470 | 2126104 | 0 | 69/69/8 | 95904/95904/14208 |
| `binary/out/256/single` | 1700 | 1700 | 588235 | 900 | 0/0/0 | 0/0/0 |
| `binary/out/256/burst64` | 38902 | 607 | 1645159 | 49902 | 0/0/4 | 11520/0/11520 |
| `binary/out/4096/single` | 1700 | 1700 | 588235 | 900 | 0/0/0 | 0/0/0 |
| `binary/out/4096/burst64` | 42602 | 665 | 1502276 | 42002 | 0/0/4 | 11520/0/11520 |
| `classified/latest` | 59803 | 934 | 1070180 | 36602 | 64/256/132 | 50124/57600/41932 |
| `classified/volatile` | 58603 | 915 | 1092094 | 76204 | 64/256/132 | 50124/57600/41932 |
| `classified/authorized-gap` | 4100 | 4100 | 243902 | 0 | 14/11/0 | 7378/6472/0 |
| `reconnect/2` | 5101 | 5101 | 196039 | 700 | 25/10/1 | 6916/6534/128 |
| `reconnect/8` | 8001 | 8001 | 124984 | 600 | 37/10/3 | 8108/8582/960 |
| `reconnect/16` | 12301 | 12301 | 81294 | 801 | 61/16/5 | 12882/14470/2624 |
| `polling/ready-frame-burst` | 15601 | 917 | 1089673 | 61204 | 17/68/3 | 7552/11492/5376 |
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

The +24-byte queue-cell growth above was accompanied by a live observation
refresh (2026-09-11, Rust 1.96.1 host) of the seven stale byte ceilings it
enlarged — `bytes_allocated`/`bytes_reallocated` for `json/out/256/burst64`,
`bytes_allocated` for `json/out/4096/burst64`, and both columns for
`classified/latest`/`classified/volatile` — which had kept their pre-growth
values, leaving 0.6–4.1 points below the documented 10% headroom (5.9% at
the thinnest, 9.4% where the growth barely bit). They now
follow the standard observed-plus-10% rule again; counts and every other
column were already current. The measured region brackets `run_measured`
only, so transport `close()`/`abort()`/flush teardown allocations remain
deliberately outside every ceiling, consistently across all cells.

The optional tenant `connect_token` field (upstream PR #575, issue #222)
then grew the `ClientMessage` value by another 24 bytes. The same queue-growth
mechanism moved +1,440 bytes in the 64-command out-burst cells and +672 in
the 17-command ready-frame cell; every operation count, protocol digest, and
semantic invariant is unchanged. Seven byte ceilings were refreshed to the
standard observed-plus-10% rule (`bytes_reallocated` for `json/out/256/burst64`
and `json/out/4096/burst64`, both byte columns for
`binary/out/256/burst64` and `binary/out/4096/burst64`, plus
`bytes_reallocated` for `polling/ready-frame-burst`), and the ready-frame
cell's `bytes_allocated` ceiling was refreshed alongside them because the
same growth had left 0.21% headroom (observed 7552, ceiling 7568); the two
`json/out` burst `bytes_allocated` columns joined the refresh for the same
reason — they had sat exactly at the rule and the growth left them at 5.3%
and 8.6%. The observed-values table above reflects the post-change live run.
