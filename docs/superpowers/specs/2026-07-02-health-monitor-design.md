# Health Monitor — Design

**Date:** 2026-07-02
**Branch:** v0.2
**Status:** Approved design; ready for implementation planning.

## Goal

The node is **healthy** when:

1. all expected Docker services are running (and, in Native mode, the host
   miner process is alive), **and**
2. it is **mining on the current active block** — i.e. our miner has
   registered a participation marker against the **current qblockid**.

We must be able to (a) query the validator directly to confirm the chain is
alive and that our node is participating, and (b) monitor the miner log to see
that it is mining. When health flips to unhealthy, we **indicate + notify**
(status pill + health panel + a tray/desktop notification). No auto-remediation.

## Background: how mining actually looks (from a live node)

Observed on a running Native-mode node (`~/quip-data-3`, miner `0.2.1rc34`) over
~6 hours of `node-output.log`, plus live validator RPC on `127.0.0.1:9944`.

There are **two independent clocks**, and conflating them is the core trap:

- **Substrate/BABE block** — the validator produces a block every **~6 s**
  (measured 1281 blocks / 128 min; confirmed live via `chain_getHeader`).
- **QUIP PoW proof / head (the qblockid)** — the miner only receives a *new
  head to mine* every **few minutes** (observed gaps of 5–13 min between
  `new head` / `participation marker` events).

**The mining log is bursty, not continuous.** On each new head the miner emits
~30 `Mining attempt` lines over ~3 s, submits a `participation marker`, then
goes quiet until the next head. Largest observed gaps between `Mining attempt`
lines: **26 min, 17 min, 17 min** — all normal (the Metal miner also has
`idle_after_s=600`). A naive "no mining line in the last N seconds → unhealthy"
check would false-alarm constantly. This is why mining-log frequency is **not**
a health gate.

The miner exposes its own staleness verdict, which we reuse:

- `event_manager … no state change for Xs (stale threshold 6.00s)` — WARNING.
- `event_manager … > dead threshold 18.00s; force-swapping` — ERROR; the miner
  gives up on its validator connection and swaps. **18 s ≈ 3 BABE blocks** is
  the node's own definition of "the chain went quiet"; we adopt it for the
  chain-liveness threshold rather than inventing one.

### The qblockid

Each head the miner mines against is identified in the log by:

- `last_proof=0x…` / `last_proof_block_hash=0x…` — the hash of the current QUIP
  proof block. **This is the qblockid.**
- `solution=N` — its monotonic sequence number (a cheap integer proxy).

The two events that matter align on that id:

```
new head (event manager):        solution=3861  last_proof=0x4d41…
participation marker submitted:   solution=3860
```

A *recent* marker is just the observable symptom of *marker.qblockid ==
current.qblockid*. But the miner's own `new head` log **freezes when the miner
is stuck or disconnected**, so comparing our marker against the *miner's* head
would falsely read healthy on a stalled node. Therefore the current qblockid
**must come from the chain**, which keeps advancing independently.

### On-chain support (from runtime metadata on the live node)

There are **no custom Quip JSON-RPC methods** (`rpc_methods` shows only standard
substrate + `state_get*`). Participation is read from pallet storage. Runtime
metadata confirms purpose-built storage exists:

- `pallet_quantum_pow`: `QBlock` / `QBlocks` (the qblockid, carries
  `last_proof_block_hash`), `ParticipantsByQBlock` (map `qblockid → participating
  miners`), `LatestParticipation`.
- `pallet_miner_registry` (`MinerRegistry`): `Miners` (per-miner registration —
  observed `deposit`, `submitted`, `won`), a `ParticipationRecord` type,
  `valid_solution_count`, `solutions_submitted`, `first_solution_at`.

So "our marker is against the current qblockid" is a **direct chain lookup**:
read the current qblockid from `QuantumPow`, then confirm our account
participated in it (via `ParticipantsByQBlock[currentQBlock]` **or** our
`ParticipationRecord`'s last qblock). Two storage reads, both chain-sourced,
immune to a stale miner log.

> **Implementation task (bounded):** pin the exact storage items and SCALE
> layouts against the live node's metadata — which of `QBlock`/`QBlocks`/
> `LatestParticipation` is the canonical "current qblockid," and the
> `ParticipationRecord` field order. Capture real `state_getStorage` responses
> as decode fixtures.

## Health model

Health is the AND of three independent dimensions, each on its own timescale.

| # | Dimension | Source | Healthy when | Threshold |
|---|-----------|--------|--------------|-----------|
| **A** | Infrastructure up | existing `roll_up_health` over mode-filtered `expected_services` (Docker) + PID check (Native miner) | all expected containers running; in Native, miner process alive | already implemented |
| **B** | Chain live & connected | validator RPC `chain_getHeader` + `system_health` | block number advanced since last poll; `isSyncing=false`; `peers>0` | expect advance within **~18 s** |
| **C** | Participating in current qblock | validator RPC storage reads (`QuantumPow` current qblock + our participation) | our account participated in the **current** qblockid, **or** the immediately-previous one during the brief post-new-head window before our marker lands (observed ~3 s) | identity match, within-1 during transition; **startup grace** before first participation |

**Dimension A applies in both modes.** In Native mode the miner is a host
process, but validator + dashboard + postgres + caddy still run in Docker; a
stalled/exited validator container drags health down in either mode.
`expected_services` already excludes the miner container in Native.

**Overall verdict (worst-wins, with debounce):**

- **Healthy** — A ∧ B ∧ C.
- **Degraded** — A up but B or C failing, or within the **startup grace**
  window before first participation. The normal post-new-head transition
  (marker one behind while the current head is younger than the observed submit
  latency) stays **Healthy** — it must not flicker the pill every few minutes.
- **Unhealthy** — infrastructure down, **or** chain stalled (B fails for 2
  consecutive polls), **or** provably not participating in the current qblockid
  (marker ≥2 behind, or 1 behind past the transition window) for 2 consecutive
  polls past the grace window.
- **Stopped** — miner not running (existing semantics; suppressed while
  starting/stopping, matching today's status pill).

**The log is the diagnostic layer, not a fourth gate.** It supplies the
human-readable "why" (`force-swapping`, `BrokenPipeError … failover`,
mining-attempt bursts, `participation marker submitted`) and may flip C to
**Degraded early** — before the chain record confirms a miss — but it never
*upgrades* health. This satisfies "monitor the log to see if it is mining"
without letting the bursty mining log raise false alarms.

## Architecture

New module **`health.rs`** owning a `HealthMonitor` with a poll loop
(**15 s**, aligned to Dimension B), separate from the 30-min `update.rs` loop.

Dimension checks are pure/thin functions returning a `DimensionStatus { state,
detail }`:

- `check_infra(run_mode, stack_status, native_status) -> DimensionStatus`
  — reuses `roll_up_health` + native PID.
- `check_chain(prev_header, now_header, sys_health) -> DimensionStatus`
  — no I/O; pure over two headers + health.
- `check_participation(current_qblock, our_record, grace_state) ->
  DimensionStatus` — no I/O; pure over decoded chain state.

A thin **`validator_rpc.rs`** helper does the I/O: a JSON-RPC client over the
`ws→http`-converted validator URL (reusing the conversion already in
`native.rs`) exposing `chain_getHeader`, `system_health`, and `state_getStorage`
+ minimal SCALE decode for the two `QuantumPow` / `MinerRegistry` items.

The monitor rolls the three dimensions into an overall `HealthReport {
overall, infra, chain, participation, reasons: Vec<String> }`, stores the latest
in shared state, emits a `health-changed` Tauri event on transition, and backs a
new `get_health` command.

**Frontend:** fold `overall` into the existing status pill
(`statusFromStack` gains a health input) and add a **health panel** listing the
three dimensions with their `detail` reason strings. Polling reuses the event +
a `get_health` fallback poll, consistent with the existing 10 s status poll.

## Notify

On a transition **into Unhealthy**, debounced over **2 consecutive polls** (to
ride out the normal ~18 s head transition without flapping), fire a
tray/desktop notification naming the failing dimension and reason. Transitions
back to Healthy clear it. No auto-restart, no re-pull.

## Error handling

- Validator RPC unreachable → Dimension B/C report `Unknown` with the transport
  error as `detail`; overall goes **Degraded** (not a hard Unhealthy) unless
  Dimension A also fails, since a transient RPC blip shouldn't page the user.
- Storage item not found / SCALE decode failure → `Unknown` + logged detail;
  never panics the loop.
- Startup grace: until the first successful participation read (or a bounded
  grace window, e.g. one head interval) Dimension C is **Degraded/"warming up,"**
  never Unhealthy.

## Testing

- **Roll-up:** table-driven tests over every A/B/C state combination → expected
  overall verdict, including grace/debounce transitions.
- **`check_chain`:** synthetic header pairs — advanced, stalled, syncing,
  zero-peers — assert the verdict.
- **`check_participation`:** decoded-state fixtures — participating in current
  qblock, one behind (transition), stalled two behind, pre-first-participation.
- **SCALE decode:** real `state_getStorage` byte fixtures captured from the live
  node → known qblockid / participation record.
- **Debounce/notify:** a single failing poll does **not** notify; two
  consecutive do; recovery clears.

## Out of scope

- Auto-remediation (restart / re-pull) — deliberately excluded (indicate +
  notify only).
- On-chain per-miner reward/economics tracking beyond the participation check.
- Historical health charting.
