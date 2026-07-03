# Restart-to-Update + Update Notification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The 30-min update monitor detects node updates (Docker image / native binary) and fires one deduped desktop notification per new version; a user-initiated "Restart to Update" button applies the update and restarts the node. The node is stopped only by explicit user action — the automatic background restart is removed.

**Architecture:** Remove the `auto_update_enabled` auto-restart entirely. The monitor keeps emitting `image-update-available`/`binary-update-available` events for the UI and adds a deduped desktop notification. A new mode-aware `restart_to_update` backend command does stop→apply→start, reusing existing compose/native functions. The frontend tracks a persistent "update available" state, swaps the `btn-start` slot to "Restart to Update" (running) / "Start Node" (stopped) — both applying the update — and shows a badge.

**Tech Stack:** Rust (Tauri v2 backend), `tauri-plugin-notification` (already present), vanilla JS/HTML frontend.

## Global Constraints

- License headers unchanged; edits stay within existing files' first-line SPDX convention.
- ≤100 lines/function, cyclomatic complexity ≤8, ≤5 positional params, ≤100-char lines, absolute imports (`crate::…`), zero clippy warnings. Run `cargo` from `src-tauri/`.
- No `config.toml` schema changes, no image-tag changes, no change to the 30-min interval.
- **App-manager updates stay out of scope** — `app-update-available` keeps its existing tray badge (`update.rs:293-298`); the node button and node-update notification never reflect an app-manager update.
- Removing the `auto_update_enabled` field needs **no migration**: `AppSettings` does not use `deny_unknown_fields`, so old `app-settings.json` files carrying the key deserialize fine (the key is ignored).
- Reuse the existing notification idiom from `health.rs:9,331-335`: `use tauri_plugin_notification::NotificationExt;` then `app.notification().builder().title(..).body(..).show()`.

### Confirmed signatures (verified against the tree 2026-07-03)

- `update.rs:281` `pub async fn background_update_monitor(app: tauri::AppHandle)` — 30-min loop; app-update block `293-298`, image loop `304-318`, **auto-apply blocks to remove** `320-338` (Docker) and `346-360` (Native), `auto_update_restart_stack` helper `369-374`.
- `update.rs:201` `check_gitlab_image_update(image: ImageRef) -> Result<Option<ImageUpdateInfo>, String>`; `ImageUpdateInfo { current_digest, latest_digest, update_available }` (`update.rs:14`); `ImageRef` (`update.rs:139`) with `display_name()`.
- `relevant_images(&settings) -> impl Iterator<Item = ImageRef>` (used at `update.rs:305`).
- `native::check_binary_update() -> Result<Option<crate::update::UpdateInfo>, String>` (`native.rs:712`); `UpdateInfo { version, url, notes }` (`update.rs:7`).
- Apply functions (all `async`, take `AppHandle` unless noted):
  - `compose::stop_stack(AppHandle)`, `compose::pull_compose_images(AppHandle)`, `compose::start_stack(AppHandle)` (`compose.rs:904,475,586`).
  - `native::stop_native_node(AppHandle, State<NativeProcessState>)`, `native::start_native_node(AppHandle, State<NativeProcessState>) -> Result<String,String>` (`native.rs:927,717`), `native::download_native_binary(AppHandle) -> Result<String,String>` (`native.rs:499`, self-guards: only downloads if strictly newer).
- `crate::native::NativeProcessState` (managed; obtained in a command via a `State<'_, NativeProcessState>` param, as `get_native_node_status` does).
- `settings.rs:383-384` `auto_update_enabled` field + default `399`.
- `lib.rs:71` `generate_handler!` list; `update::background_update_monitor` spawned at `lib.rs:233`.
- Frontend: `state` object (`app.js:18`); `updateStartStopState()` (`app.js:745-755`); btn-start click (`app.js:1273`); event listeners (`app.js:1515-1524`); `startNode()`/`stopNode()` (`app.js:1249,1262`); btn-stop already sets `state.health=null` (`app.js:1304`). Buttons `index.html:55-56`; auto-update checkbox `index.html:186-190`; frontend `auto_update_enabled` bindings `app.js:641,1656`.

## File Structure

- **Modify `src-tauri/src/settings.rs`** — remove `auto_update_enabled`.
- **Modify `src-tauri/src/update.rs`** — remove auto-restart; add deduped notification; add `restart_to_update` command + `has_new_update`/step helpers.
- **Modify `src-tauri/src/lib.rs`** — register `restart_to_update`.
- **Modify `src/index.html`** — remove auto-update checkbox; add update badge.
- **Modify `src/app.js`** — remove `auto_update_enabled` bindings; add update state, badge, button swap, restart wiring.

---

### Task 1: Remove the auto-restart and the `auto_update_enabled` setting

**Files:**
- Modify: `src-tauri/src/settings.rs:383-384,398-399`
- Modify: `src-tauri/src/update.rs:301-362,366-374`
- Modify: `src/index.html:184-191`
- Modify: `src/app.js:641,1656`

**Interfaces:**
- Produces: a monitor loop (`background_update_monitor`) that still emits `image-update-available` / `binary-update-available` but performs NO stop/pull/start/download. `auto_update_restart_stack` is deleted. `AppSettings` no longer has `auto_update_enabled`.

- [ ] **Step 1: Remove the setting field**

In `settings.rs` delete the field (lines 383-384):
```rust
    #[serde(default)]
    pub auto_update_enabled: bool,
```
and its initializer in `Default` (line 399): remove the `auto_update_enabled: false,` line.

- [ ] **Step 2: Strip the auto-apply from the monitor**

In `update.rs` `background_update_monitor`, delete the Docker auto-apply block (lines 320-338, the `if any_compose_update && settings.auto_update_enabled { … auto_update_restart_stack … }`) and the Native auto-download block (lines 346-360, the inner `if settings.auto_update_enabled { … download_native_binary … }`), keeping the surrounding event emits. After edit, the Native section reads:
```rust
        if settings.run_mode == crate::settings::RunMode::Native {
            if let Ok(Some(info)) = crate::native::check_binary_update().await {
                let _ = app.emit("binary-update-available", &info);
            }
        }
```
The image loop keeps emitting `image-update-available`; drop the now-unused `any_compose_update` accumulator.

- [ ] **Step 3: Delete the dead helper**

Delete `auto_update_restart_stack` (`update.rs:369-374`) — it now has no caller. (Task 3 builds the user-initiated restart fresh.)

- [ ] **Step 4: Remove the frontend checkbox + bindings**

In `index.html` delete the auto-update toggle block (lines 184-190, the `<!-- Auto-update toggle -->` div through its closing `</div>`).
In `app.js` delete the two `auto_update_enabled` references: the write in `applyFormToSettings` (line 641, `state.settings.auto_update_enabled = …`) and the populate line (line 1656, `document.getElementById('auto-update-enabled').checked = …`).

- [ ] **Step 5: Verify build/tests + no dangling references**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets`
Expected: builds clean, all existing tests pass, zero warnings (no unused-variable warning for the removed accumulator/helper).
Run: `rg -n 'auto_update_enabled|auto_update_restart_stack|auto-update-enabled' src-tauri/src src/` → expected: no matches.
Run: `node --check src/app.js` → clean.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/settings.rs src-tauri/src/update.rs src/index.html src/app.js
git commit -m "refactor(update): remove auto-restart and auto_update_enabled setting"
```

---

### Task 2: Deduplicated desktop notification in the monitor

**Files:**
- Modify: `src-tauri/src/update.rs`

**Interfaces:**
- Consumes: the cleaned monitor loop from Task 1.
- Produces: `has_new_update(current: &HashSet<String>, last: &HashSet<String>) -> bool` (pure); the monitor fires one desktop notification when a genuinely new update id appears, then records `last_notified = current`.

- [ ] **Step 1: Write the failing test**

Add near the `update.rs` `tests` module:
```rust
    #[test]
    fn notifies_only_on_a_newly_appeared_update() {
        use std::collections::HashSet;
        let none: HashSet<String> = HashSet::new();
        let a: HashSet<String> = ["miner:sha_a".into()].into();
        let ab: HashSet<String> = ["miner:sha_a".into(), "binary:0.2.1".into()].into();

        assert!(has_new_update(&a, &none), "first detection notifies");
        assert!(!has_new_update(&a, &a), "same set: no re-nag");
        assert!(has_new_update(&ab, &a), "a newly added id notifies");
        assert!(!has_new_update(&a, &ab), "shrinking set does not notify");
        assert!(!has_new_update(&none, &a), "cleared set does not notify");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test notifies_only_on_a_newly_appeared_update`
Expected: FAIL — `has_new_update` not found.

- [ ] **Step 3: Implement the pure helper + notification**

At the top of `update.rs` add `use std::collections::HashSet;` and `use tauri_plugin_notification::NotificationExt;` (with the other imports).

Add the pure helper:
```rust
/// True when `current` contains an update id not present in `last` — i.e. a
/// genuinely new update appeared since the last notification. Shrinking or
/// clearing the set never notifies (an already-notified or applied update must
/// not re-nag).
fn has_new_update(current: &HashSet<String>, last: &HashSet<String>) -> bool {
    current.difference(last).next().is_some()
}

fn notify_update_available(app: &tauri::AppHandle) {
    let _ = app
        .notification()
        .builder()
        .title("Quip node update available")
        .body("Restart to Update to apply the latest node update.")
        .show();
}
```

In `background_update_monitor`, declare the dedup memory BEFORE the `loop {` (persists across polls, resets on app restart):
```rust
    let mut last_notified: HashSet<String> = HashSet::new();
```
Inside the loop, collect pending-update ids alongside the existing emits. In the image loop, when `info.update_available`, after the `app.emit("image-update-available", …)`, insert an id:
```rust
                    current.insert(format!("{}:{}", image.display_name(), info.latest_digest));
```
(declare `let mut current: HashSet<String> = HashSet::new();` just before the image loop). In the Native binary block, after `app.emit("binary-update-available", &info)`, add:
```rust
                current.insert(format!("binary:{}", info.version));
```
After both checks, notify once and record:
```rust
        if has_new_update(&current, &last_notified) {
            notify_update_available(&app);
        }
        last_notified = current;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets`
Expected: the new test and all existing tests PASS; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/update.rs
git commit -m "feat(update): deduped desktop notification when a node update appears"
```

---

### Task 3: `restart_to_update` command

**Files:**
- Modify: `src-tauri/src/update.rs`
- Modify: `src-tauri/src/lib.rs:71-114`

**Interfaces:**
- Produces:
  - `enum UpdateStep { StopNative, StopStack, DownloadBinary, PullImages, StartStack, StartNative }`
  - `fn update_restart_steps(mode: &crate::settings::RunMode) -> Vec<UpdateStep>` (pure).
  - `#[tauri::command] pub async fn restart_to_update(app) -> Result<(), String>` (native state is fetched from `app`, not passed in).

- [ ] **Step 1: Write the failing test for the step plan**

Add to `update.rs` tests:
```rust
    #[test]
    fn docker_restart_plan_is_stop_pull_start() {
        use UpdateStep::*;
        assert_eq!(
            update_restart_steps(&RunMode::Docker),
            vec![StopStack, PullImages, StartStack]
        );
    }

    #[test]
    fn native_restart_plan_stops_both_downloads_and_starts_both() {
        use UpdateStep::*;
        assert_eq!(
            update_restart_steps(&RunMode::Native),
            vec![StopNative, StopStack, DownloadBinary, PullImages, StartStack, StartNative]
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test restart_plan`
Expected: FAIL — `UpdateStep` / `update_restart_steps` not found.

- [ ] **Step 3: Implement steps, dispatch, and the command**

Add to `update.rs`:
```rust
use crate::native::NativeProcessState;
use tauri::Manager;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateStep {
    StopNative,
    StopStack,
    DownloadBinary,
    PullImages,
    StartStack,
    StartNative,
}

/// The ordered stop→apply→start plan for a user-initiated update restart.
/// Docker stops/starts only the compose stack; Native also stops/starts the
/// host miner and refreshes its binary. Each step is idempotent from a stopped
/// state, so the same plan works whether or not the node is currently running.
fn update_restart_steps(mode: &crate::settings::RunMode) -> Vec<UpdateStep> {
    use UpdateStep::*;
    match mode {
        crate::settings::RunMode::Docker => vec![StopStack, PullImages, StartStack],
        crate::settings::RunMode::Native => {
            vec![StopNative, StopStack, DownloadBinary, PullImages, StartStack, StartNative]
        }
    }
}

async fn run_update_step(app: &tauri::AppHandle, step: UpdateStep) -> Result<(), String> {
    // Native steps fetch the managed process state from `app` (same idiom as
    // health.rs), so the command needs no State parameter.
    match step {
        UpdateStep::StopNative => {
            crate::native::stop_native_node(app.clone(), app.state::<NativeProcessState>()).await
        }
        UpdateStep::StopStack => crate::compose::stop_stack(app.clone()).await,
        UpdateStep::DownloadBinary => {
            crate::native::download_native_binary(app.clone()).await.map(|_| ())
        }
        UpdateStep::PullImages => crate::compose::pull_compose_images(app.clone()).await,
        UpdateStep::StartStack => crate::compose::start_stack(app.clone()).await,
        UpdateStep::StartNative => {
            crate::native::start_native_node(app.clone(), app.state::<NativeProcessState>())
                .await
                .map(|_| ())
        }
    }
}

/// User-initiated: stop → apply the pending update → start, mode-aware. Bails on
/// the first failing step (leaving the node stopped) rather than claiming a
/// false success — the caller keeps the update flagged and re-enables the button.
#[tauri::command]
pub async fn restart_to_update(app: tauri::AppHandle) -> Result<(), String> {
    let mode = crate::settings::load_settings().run_mode;
    for step in update_restart_steps(&mode) {
        run_update_step(&app, step).await?;
    }
    Ok(())
}
```

- [ ] **Step 4: Register the command**

In `lib.rs` add `update::restart_to_update,` to the `generate_handler!` list (near the other `update::` entries, ~line 114).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd src-tauri && cargo test && cargo clippy --all-targets`
Expected: the two plan tests + all existing tests PASS; builds clean.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/update.rs src-tauri/src/lib.rs
git commit -m "feat(update): user-initiated restart_to_update command (mode-aware)"
```

---

### Task 4: Frontend — update state, badge, button swap, restart wiring

**Files:**
- Modify: `src/index.html:48-58`
- Modify: `src/app.js`

**Interfaces:**
- Consumes: `restart_to_update` command; `image-update-available` / `binary-update-available` events.

- [ ] **Step 1: Add update state fields**

In the `state` object (`app.js:18`), add:
```js
  updateAvailable: null,   // null | { kind: 'image' | 'binary' }
  updating: false,
```

- [ ] **Step 2: Add the badge element**

In `index.html`, inside the status-section, add a badge next to the status text (after the `status-subtext` div, ~line 53):
```html
            <span id="update-badge" title="A node update is available — Restart to Update"
              style="display:none;margin-left:8px;padding:2px 8px;border-radius:10px;
              background:var(--accent);color:#fff;font-size:11px;font-weight:600;">Update</span>
```

- [ ] **Step 3: Set update state from the events**

Replace the `image-update-available` and `binary-update-available` listeners (`app.js:1515-1524`) so they set state + badge, keeping the log line:
```js
  await listen('image-update-available', () => {
    appendLog({ timestamp: '', level: 'INFO', message: 'New Docker image available. Restart to update.' });
    state.updateAvailable = { kind: 'image' };
    document.getElementById('update-badge').style.display = '';
    updateStartStopState();
    refreshNodeVersion();
  });

  await listen('binary-update-available', (event) => {
    const info = event.payload;
    appendLog({ timestamp: '', level: 'INFO', message: `New native miner v${info.version} available. Restart to update.` });
    state.updateAvailable = { kind: 'binary' };
    document.getElementById('update-badge').style.display = '';
    updateStartStopState();
    refreshNodeVersion();
  });
```

- [ ] **Step 4: Make the button swap update-aware**

Replace `updateStartStopState()` (`app.js:745-755`) with:
```js
function updateStartStopState() {
  const running = state.containerRunning || state.nativeRunning;
  const startBtn = document.getElementById('btn-start');
  const stopBtn = document.getElementById('btn-stop');
  const pendingUpdate = !!state.updateAvailable;

  if (state.updating) {
    startBtn.textContent = 'Updating…';
    startBtn.disabled = true;
  } else if (pendingUpdate && running) {
    // Node running + update pending: btn-start becomes the apply action.
    startBtn.textContent = 'Restart to Update';
    startBtn.disabled = !state.checksPassed;
  } else {
    // Stopped (with or without update) or running with no update: normal Start.
    startBtn.textContent = state.starting ? 'Starting…' : 'Start Node';
    startBtn.disabled = !state.checksPassed || running || state.starting || state.stopping;
  }

  stopBtn.textContent = state.stopping ? 'Stopping…' : 'Stop Node';
  stopBtn.disabled = !running || state.starting || state.stopping || state.updating;
  document.getElementById('btn-apply').disabled =
    !state.checksPassed || state.starting || state.stopping || state.updating;
}
```

- [ ] **Step 5: Route the btn-start click through the update flow**

At the top of the `btn-start` click handler (`app.js:1273`), branch to the update path when an update is pending:
```js
document.getElementById('btn-start').addEventListener('click', async () => {
  if (state.updateAvailable) {
    await runRestartToUpdate();
    return;
  }
  // …existing start logic unchanged…
```
Add the helper (near `startNode`, ~`app.js:1249`):
```js
async function runRestartToUpdate() {
  const applyStatus = document.getElementById('apply-status');
  state.updating = true;
  updateStartStopState();
  applyStatus.textContent = 'Updating…';
  appendLog({ timestamp: '', level: 'INFO', message: 'Restarting node to apply update…' });
  try {
    if (state.settings) { applyFormToSettings(); await invoke('update_settings', { settings: state.settings }); }
    await invoke('restart_to_update');
    state.updateAvailable = null;
    document.getElementById('update-badge').style.display = 'none';
    applyStatus.textContent = 'Node updated and started.';
    await pollStatus();
  } catch (e) {
    // Keep the update flagged so the button stays actionable.
    applyStatus.textContent = `Error: ${e}`;
    appendLog({ timestamp: '', level: 'ERROR', message: `Update failed: ${e}` });
  } finally {
    state.updating = false;
    updateStartStopState();
  }
}
```

- [ ] **Step 6: Verify**

Run: `node --check src/app.js` → clean.
Re-read the diff: confirm (a) stopped+update still shows "Start Node" but click runs `runRestartToUpdate`; (b) running+update shows "Restart to Update"; (c) `updating` disables all three buttons; (d) success clears `state.updateAvailable` + hides the badge, failure keeps them.

- [ ] **Step 7: Commit**

```bash
git add src/index.html src/app.js
git commit -m "feat(update): Restart-to-Update button, update badge, restart wiring"
```

---

### Task 5: Manual smoke test

**Files:** none (verification).

- [ ] **Step 1: Build and launch**

Run: `cd src-tauri && cargo test` (green), then `bun run dev`.

- [ ] **Step 2: No-update baseline**

With no update pending, confirm the Start/Stop buttons behave exactly as before (Start when stopped, Stop when running, Apply & Restart unchanged), and no "Update" badge shows.

- [ ] **Step 3: Simulate an update-available event**

In the app devtools console, emit the event the monitor would send (or wait for a real one): confirm the **Update badge** appears, a **desktop notification** fires once, and — if the node is running — `btn-start` becomes **"Restart to Update"**; if stopped, it stays **"Start Node"**.

- [ ] **Step 4: Apply**

Click the button. Confirm it shows **"Updating…"** (all buttons disabled), runs stop→pull/download→start (watch the log + native download progress in Native mode), then clears the badge and returns to normal Start/Stop with the node running. Re-detection on the next poll does not re-notify.

- [ ] **Step 5: Commit any fixes**

Fix glue bugs with a focused commit if needed.

---

## Self-Review

**Spec coverage:**
- Monitor detect+notify only, never stops the node → Task 1 (remove auto-restart) + Task 2 (notify). ✓
- Deduped desktop notification per new version → Task 2 `has_new_update`. ✓
- Remove `auto_update_enabled` + checkbox + Docker auto-restart + Native auto-download → Task 1. ✓
- `restart_to_update` mode-aware (Docker stop→pull→start; Native stop-both→download+pull→start-both) → Task 3. ✓
- Button swap in `btn-start` slot: "Restart to Update" (running) / "Start Node" (stopped), both apply; "Updating…" state → Task 4. ✓
- Persistent update state + badge; clears on success, kept on failure → Task 4. ✓
- Error handling surfaces + keeps button actionable → Task 4 `runRestartToUpdate` catch. ✓
- App-manager updates out of scope → untouched `app-update-available` path. ✓
- Testing: dedup decision + mode-branch plan unit-tested; full apply smoke-tested → Tasks 2, 3, 5. ✓

**Placeholder scan:** none. The `State::clone()` note in Task 3 is a concrete compiler-resolved fallback, not a placeholder.

**Type consistency:** `state.updateAvailable`/`state.updating`, `runRestartToUpdate`, `update-badge`, `has_new_update`, `UpdateStep`/`update_restart_steps`/`run_update_step`/`restart_to_update` used consistently across tasks.

**Open at implementation:** none — all signatures verified above; Task 3 fetches native state via `app.state::<NativeProcessState>()` (proven idiom in `health.rs`), so there is no `State`-threading unknown.
