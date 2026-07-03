# Restart-to-Update + Update Notification — Design

**Date:** 2026-07-03
**Branch base:** v0.2
**Status:** Approved design; ready for implementation planning.

## Goal

Notify the user when a node update (Docker image or native miner binary) is
available, and give them a one-click **"Restart to Update"** button that applies
the update and restarts the node — **only when they choose to**.

Guiding principle: **the node is stopped only by explicit user action.** Applying
an update is safe anytime (it takes effect on the next start — a fresh
`docker compose up` pulls, a fresh native spawn uses the downloaded binary), but
**stopping a running node is disruptive and must stay user-initiated.** The
background monitor therefore only detects and notifies; it never stops or
restarts the node.

## Background: what already exists

- **Background update monitor** (`update.rs:281`, every 30 min): checks
  miner/validator/dashboard **image digests** (Docker) and the **quip-miner
  binary** via GitLab releases (Native), plus app-manager releases. Emits
  `image-update-available` (`update.rs:309`), `binary-update-available`
  (`update.rs:344`), `app-update-available` (`update.rs:293`).
- **Apply paths:** `auto_update_restart_stack()` (`update.rs:369`) does
  stop→pull→start for Docker; `download_native_binary()` (`native.rs:499`)
  downloads the binary but does not restart. Building blocks:
  `compose::pull_compose_images()` (`compose.rs:475`),
  `compose::start_stack()` (`compose.rs:586`), `compose::stop_stack()`
  (`compose.rs:904`), `native::start_native_node`/`stop_native_node`,
  `native::download_native_binary()`.
- **`auto_update_enabled`** setting (`settings.rs:384`, default false): when on,
  the monitor auto-runs `auto_update_restart_stack()` (Docker) and
  `download_native_binary()` (Native). Checkbox at `index.html:188`
  ("Auto-restart when new image available").
- **Frontend today** only writes a **log line** on `image-update-available` /
  `binary-update-available` (`app.js:1515,1520`); no persistent state, no
  notification. App updates get a tray badge (`lib.rs:41 set_tray_update`,
  `update.rs:294`).
- **Buttons:** `btn-start` / `btn-stop` (`index.html:55-56`), state driven by
  `updateStartStopState()` (`app.js:745`). Start only enabled when stopped,
  Stop only when running.
- **`tauri-plugin-notification`** is already registered (`lib.rs:64`) and used by
  the health monitor (`health.rs` `NotificationExt`).

## What changes

### 1. Background monitor → detect + notify only (`update.rs`)

**Remove the auto-restart entirely** (replace, don't deprecate):

- Delete `auto_update_restart_stack()` (`update.rs:369`) and its invocation
  (`update.rs:320`).
- Delete the Native background auto-download invocation (`update.rs:354`).
- Delete the `auto_update_enabled` field (`settings.rs:384,399`), its checkbox
  (`index.html:188`), and all frontend bindings (`app.js:641,1656`).
  `download_native_binary()` itself stays — it is reused by `restart_to_update`.

The monitor keeps detecting updates and emitting the existing
`image-update-available` / `binary-update-available` events for the UI. It adds
**one deduplicated desktop notification** per new update:

- Hold the last-notified state in the monitor loop: the set of image digests it
  last notified for, and the last-notified binary version.
- A pure decision `should_notify(current, last_notified) -> bool`: notify when a
  detected update's digest/version differs from what was last notified. After
  notifying, record the new digest/version so subsequent 30-min polls do **not**
  re-nag; re-notify only when a newer version appears.
- Fire the notification via `NotificationExt` (same path as `health.rs`), title
  e.g. "Quip node update available", body "Restart to Update to apply."

**App-manager updates stay out of scope** — they keep their existing tray badge
(`app-update-available` → `set_tray_update`). The node button never reflects an
app-manager update (restarting the node cannot update the manager).

### 2. `restart_to_update` command — user-initiated, mode-aware (backend)

A new `#[tauri::command] async fn restart_to_update(app) -> Result<(), String>`.
It internally "stops if running", so it works from either running or stopped
state. Mode branch (extract the branch selection as a pure, testable helper):

- **Docker:** `stop_stack → pull_compose_images → start_stack` (the sequence the
  removed `auto_update_restart_stack` used).
- **Native:** `stop_native_node` + `stop_stack` → `download_native_binary()` (if
  a binary update is pending) + `pull_compose_images()` (validator/dashboard
  images) → `start_stack` + `start_native_node`.

Reuses the existing log/status/`starting`/`stopping` events and the binary
download-progress events already emitted by `download_native_binary`.

### 3. Frontend — persistent update state + button swap

- Listen for `image-update-available` / `binary-update-available` → set
  `state.updateAvailable = { kind, version }` and render a **badge/dot near the
  status pill**. (Keep the existing log line.)
- Extend `updateStartStopState()` (`app.js:745`): the swap happens in the
  **`btn-start` slot** (`index.html:55`). When `state.updateAvailable` is set,
  that button's click calls `restart_to_update` instead of the normal start:
  - **Running** → `btn-start` becomes the enabled, highlighted **"Restart to
    Update"** (it is otherwise disabled while running); `btn-stop` stays the
    plain **"Stop Node"**. Layout: `[ Restart to Update ] [ Stop Node ]`.
  - **Stopped** → `btn-start` keeps the label **"Start Node"**, but its click
    calls `restart_to_update` (already stopped, so it just pulls/downloads →
    starts); `btn-stop` stays disabled. Layout: `[ Start Node ] [ Stop Node(off) ]`.
  - During apply, `btn-start` shows an **"Updating…"** disabled state (mirrors
    the existing `starting`/`stopping` handling), with `btn-stop` disabled too.
- Clear `state.updateAvailable` after a successful restart-to-update; the next
  monitor poll finds matching digests and emits nothing.
- No pending update → today's Start/Stop behavior, unchanged.

### 4. Error handling

If pull / download / restart fails: surface the error via the existing
log/status path, **keep** `state.updateAvailable` set (button stays actionable),
and re-enable the button. No silent failure; no partial "success" claim.

## Testing

- **Backend unit tests:**
  - `should_notify` dedup decision: new digest/version → true; same → false;
    newer → true; first-ever → true.
  - The `restart_to_update` **mode-branch selection** (pure helper) → correct
    step list per `RunMode`.
- **Backend integration:** full `restart_to_update` flow is exercised by the
  manual smoke test (stop→pull/download→start in each mode).
- **Frontend:** no JS test runner in this project — verify the button
  state-machine transitions (running/stopped × update/no-update × updating) by
  review.

## Relationship to prior behavior

The `auto_update_enabled` automatic-restart is **removed**, not kept behind a
flag. Users who previously relied on auto-restart now get a notification + the
manual button instead — an intentional behavior change matching the
"user controls when the node stops" principle.

## Out of scope

- App-manager self-update (keeps its existing tray badge / release link).
- Changing the 30-min monitor interval or the update-detection mechanics.
- Rollback / downgrade (updates only move forward, as today).
