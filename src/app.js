// SPDX-License-Identifier: AGPL-3.0-or-later

// Tauri IPC bridge
const invoke =
  window.__TAURI__?.core?.invoke ??
  (() => Promise.reject('Tauri not available'));
const listen =
  window.__TAURI__?.event?.listen ?? (() => Promise.resolve(() => {}));
// Route external URLs through the tauri-plugin-opener Rust command.
// window.__TAURI__.opener.openUrl only exists if the plugin's JS wrapper
// is bundled into the frontend (we ship raw JS, so it isn't). invoke()
// goes straight to the Rust side, which shells out to `open(1)` / etc.
const openUrl = (url) =>
  invoke('plugin:opener|open_url', { url })
    .catch((e) => console.error('openUrl failed:', e));

// App state
const state = {
  settings: null,
  containerRunning: false,
  nativeRunning: false,
  // Full StackStatus returned by get_stack_status:
  // { services: [{name, service, running, health, status_text, image}], overall }
  stack: null,
  // HealthReport returned by get_health / health-changed event:
  // { overall, infra:{state,detail}, chain:{state,detail}, participation:{state,detail} }
  health: null,
  checksPassed: false,
  starting: false,
  stopping: false,
  updateAvailable: null,   // null | { kind: 'image' | 'binary' }
  updating: false,
  detectedGpus: [], // { index, name }
  logLines: [],
  MAX_LOG_LINES: 500,
  pollInterval: null,
  // Map<id, CheckItem> — single source of truth for the checklist UI.
  // Merged from `checklist-update` events; rendered by renderChecklist().
  checks: new Map(),
  hardwareSurvey: null,
  // Set by the `dashboard-db-mismatch` event when Postgres rejects the
  // dashboard's password; cleared once the dashboard comes up. Drives the
  // error message + Reset button in the dashboard placeholder.
  dashboardDbError: null,
  // Highest pull generation that has received its `pull-complete`. Progress
  // events at or below this are stale (a late layer event from a finished
  // pull) and must not reopen the panel. See handlePullProgress.
  pullCompletedGen: 0,
};

const DEFAULT_DASHBOARD_HOSTNAME = ':20049';
const CADDY_PUBLIC_API_PORT = 20049;

function publicHostName(value) {
  let host = (value || '').trim();
  if (!host) return '';
  const schemeIndex = host.indexOf('://');
  if (schemeIndex >= 0) host = host.slice(schemeIndex + 3);
  if (host.includes('@')) host = host.split('@').pop();
  host = host.split(/[/?#]/)[0].trim();
  if (host.startsWith('[')) {
    host = host.slice(1).split(']')[0].trim();
  } else if ((host.match(/:/g) || []).length === 1) {
    host = host.split(':')[0].trim();
  }
  return /\s/.test(host) ? '' : host.replace(/\.$/, '');
}

function isIpAddress(hostname) {
  return /^(\d{1,3}\.){3}\d{1,3}$/.test(hostname) ||
    (hostname.includes(':') && /^[0-9a-f:]+$/i.test(hostname));
}

function isPublicDnsHost(hostname) {
  if (!hostname || isIpAddress(hostname)) return false;
  const lower = hostname.toLowerCase();
  return lower !== 'localhost' &&
    !lower.endsWith('.localhost') &&
    !lower.endsWith('.local');
}

function dashboardHostnameForSettings(settings) {
  const publicHost = publicHostName(settings?.node_config?.public_host);
  if (isPublicDnsHost(publicHost)) {
    return `${publicHost}, ${publicHost}:${CADDY_PUBLIC_API_PORT}`;
  }
  // Mirror resolved_caddy_hostname: only honor the hostname field when it holds
  // a real public DNS host (incl. the `host, host:20049` form). localhost / an
  // IP / a bare or edited port normalize to the port-only default so the iframe
  // targets plain HTTP on the (remapped) API port instead of an auto-HTTPS host.
  const fallback = (settings?.hostname || '').trim();
  const fallbackHost = publicHostName(fallback.split(',')[0]);
  if (isPublicDnsHost(fallbackHost)) {
    return fallback;
  }
  return DEFAULT_DASHBOARD_HOSTNAME;
}

function dashboardHostForBrowser(settings) {
  const configured = dashboardHostnameForSettings(settings);
  const firstHost = (configured || DEFAULT_DASHBOARD_HOSTNAME)
    .split(',')[0]
    .trim() || DEFAULT_DASHBOARD_HOSTNAME;
  if (firstHost === DEFAULT_DASHBOARD_HOSTNAME) {
    return `localhost:${settings?.node_config?.port || CADDY_PUBLIC_API_PORT}`;
  }
  return firstHost.startsWith(':') ? `localhost${firstHost}` : firstHost;
}

function isLocalDashboardHost(hostname) {
  return hostname.startsWith('localhost') ||
    hostname.startsWith('127.0.0.1') ||
    hostname.startsWith('[::1]');
}

// ─── Stack Configuration UI ─────────────────────────────────────────────────

// Show certificate subsettings only when TLS is enabled. The dashboard is
// always part of the v0.2 stack.
function updateStackUiVisibility() {
  const tlsEl = document.getElementById('tls-enabled');
  const subs = document.getElementById('tls-subsettings');
  if (!tlsEl || !subs) return;

  subs.style.display = tlsEl.checked ? '' : 'none';
}

// Show CPU subsettings (core count) only while CPU mining is enabled; the
// toggle itself governs whether config.toml gets a [cpu] section.
function updateCpuUiVisibility() {
  const cpuEl = document.getElementById('cpu-enabled');
  const subs = document.getElementById('cpu-subsettings');
  if (!cpuEl || !subs) return;

  subs.style.display = cpuEl.checked ? '' : 'none';
}

document.addEventListener('change', (e) => {
  if (!e.target) return;
  if (e.target.id === 'tls-enabled') {
    updateStackUiVisibility();
  }
  if (e.target.id === 'cpu-enabled') {
    updateCpuUiVisibility();
  }
});

// Segmented Release/Beta channel control (buttons, not form inputs).
document.addEventListener('click', (e) => {
  const btn = e.target?.closest?.('.segmented-btn[data-channel]');
  if (btn && !btn.disabled) {
    setUpdateChannel(btn.dataset.channel);
  }
});

// ─── Update channel (Release / Beta) ──────────────────────────────────────
//
// The channel selects which published tag each image + the native binary
// tracks. Every image resolves its own tag independently from its own registry
// (see src-tauri registry.rs): Release = its latest stable (no -rc); Beta = its
// latest incl. rc. Release is grayed out and forced to Beta unless every image
// has a stable tag to run.

function updateChannelButtons() {
  const ch = state.settings?.update_channel ?? 'release';
  document.getElementById('channel-release')?.classList.toggle('active', ch === 'release');
  document.getElementById('channel-beta')?.classList.toggle('active', ch === 'beta');
}

async function setUpdateChannel(channel) {
  if (!state.settings || state.settings.update_channel === channel) return;
  state.settings.update_channel = channel;
  updateChannelButtons();
  await invoke('update_settings', { settings: state.settings }).catch(console.error);
  await refreshChannelInfo();
}

async function refreshChannelInfo() {
  if (!state.settings) return;
  const caption = document.getElementById('channel-caption');
  const releaseBtn = document.getElementById('channel-release');
  let info;
  try {
    info = await invoke('resolve_channel_info', {
      channel: state.settings.update_channel,
    });
  } catch (e) {
    if (caption) caption.textContent = 'Could not reach the release feed.';
    return;
  }
  // Gray out Release until a stable (non-rc) release exists; force Beta.
  if (releaseBtn) {
    releaseBtn.disabled = !info.stable_available;
    releaseBtn.title = info.stable_available
      ? ''
      : 'No stable release published yet — Beta only';
  }
  if (!info.stable_available && state.settings.update_channel === 'release') {
    await setUpdateChannel('beta');
    return;
  }
  updateChannelButtons();
  if (caption) {
    // Each image resolves its own tag from its own registry — they can differ.
    const parts = (info.images || []).map(
      ([name, tag]) => `${name} ${tag ?? '—'}`,
    );
    caption.textContent = parts.length
      ? parts.join('  ·  ')
      : 'No published builds on this channel yet.';
  }
}

// ─── Dashboard tab iframe wiring ────────────────────────────────────────────

function dashboardUrl(settings) {
  const hostname = dashboardHostForBrowser(settings);
  // ACME via Caddy only when TLS is on AND the hostname is a real DNS name
  // (localhost can't get a public cert). In every other case plain HTTP on
  // whatever port was configured.
  if (settings.tls_enabled && !isLocalDashboardHost(hostname)) {
    return `https://${hostname.replace(/:.*/, '')}`;
  }
  return `http://${hostname}`;
}

function refreshDashboardTab() {
  const frame = document.getElementById('dashboard-frame');
  const empty = document.getElementById('dashboard-empty');
  const msg = document.getElementById('dashboard-empty-msg');
  const resetBtn = document.getElementById('dashboard-reset-btn');
  if (!frame || !empty) return; // tab markup not present on first load

  const url = dashboardUrl(state.settings);
  // Only load the iframe once the dashboard is actually serving: running AND
  // (healthy or no healthcheck). Loading it while the dashboard/Caddy are still
  // starting lands the iframe on a connection-error page that never recovers —
  // refreshDashboardTab won't re-set an unchanged `src`, so it stays stuck
  // until a manual reload. The dashboard image ships a healthcheck, and Caddy
  // starts after it, so "healthy" implies the front door is up.
  const dashSvc = state.stack?.services?.find((s) => s.service === 'dashboard');
  const dashRunning = !!dashSvc?.running
    && dashSvc.health !== 'starting'
    && dashSvc.health !== 'unhealthy';

  // Toggle via style.display rather than the `hidden` attribute — the
  // placeholder has `display: flex` in CSS (for its centered layout) which
  // would otherwise override `hidden` and keep both elements visible.
  const show = (el, display) => { el.style.display = display; };

  // Compare against the raw content attribute, not the IDL `frame.src`
  // getter — the latter returns the URL-normalized form (trailing slash
  // appended to `http://host:port`) and would differ from our computed
  // string on every tick, reloading the iframe and resetting the user's
  // place on the page.
  const currentSrc = frame.getAttribute('src');
  if (!dashRunning) {
    // Placeholder whenever the dashboard isn't up. The Reset button is always
    // offered here so the operator can recreate the dashboard DB on demand
    // (e.g. to clear a stale cached identity), not only on a detected
    // password mismatch. Only the mismatch detail is shown (when present); no
    // generic "starting" text.
    if (msg) msg.textContent = state.dashboardDbError || '';
    if (resetBtn) show(resetBtn, 'inline-block');
    show(empty, 'flex');
    show(frame, 'none');
    if (currentSrc !== 'about:blank') frame.src = 'about:blank';
  } else {
    // The dashboard is up — any earlier password mismatch is resolved.
    state.dashboardDbError = null;
    if (resetBtn) show(resetBtn, 'none');
    if (currentSrc !== url) frame.src = url;
    show(empty, 'none');
    show(frame, 'block');
  }
}

// Render order; the backend decides visibility, so this array can safely
// include ids that don't appear in state.checks for the current settings
// combo (they're skipped). Mirrors ALL_CHECK_IDS in checklist.rs.
const CHECK_ORDER = [
  'docker', 'docker-compose', 'wsl',
  'binary', 'secret',
  'ip', 'hostname', 'port', 'port-validator', 'dwave-key',
];

// State-to-icon mapping for the checklist. CSS class `state-<state>`
// drives colour and (for running) the spin animation.
const STATE_ICON = {
  idle:    '○', // ○
  running: '◌', // ◌
  pass:    '✓', // ✓
  warn:    '⚠', // ⚠
  fail:    '✗', // ✗
  skip:    '—', // —
};

// Fix button labels, keyed by FixKind.kind.
const FIX_LABELS = {
  InstallDocker:   'Install Docker',
  GenerateSecret:  'Generate Secret',
};

// Secret generation is folded into the Retry button: Retry generates the
// secret (only when the check is failing — regenerating it on a passing check
// would be destructive) and then rechecks, so there's no separate fix button.
// Downloads (stack images, native binary) are performed by the backend check
// itself on recheck, so their Retry is just a plain recheck.
const FIX_FOLDED_INTO_RETRY = new Set(['secret']);

// ─── Tab switching ──────────────────────────────────────────────────────────
// Activate a tab by name (e.g. 'status', 'dashboard'). Used by the tab buttons
// and by flows that need to navigate the user programmatically.
function activateTab(tab) {
  document
    .querySelectorAll('.tab-btn')
    .forEach((b) => b.classList.toggle('active', b.dataset.tab === tab));
  document
    .querySelectorAll('.tab-content')
    .forEach((c) => c.classList.toggle('active', c.id === `tab-${tab}`));
  if (state.settings) {
    state.settings.active_tab = tab;
    invoke('update_settings', { settings: state.settings }).catch(console.error);
  }
  if (tab === 'dashboard') refreshDashboardTab();
}

document.querySelectorAll('.tab-btn').forEach((btn) => {
  btn.addEventListener('click', () => activateTab(btn.dataset.tab));
});

// ─── Configuration section toggle ────────────────────────────────────────────
document.getElementById('btn-config-toggle').addEventListener('click', () => {
  const btn = document.getElementById('btn-config-toggle');
  const section = document.getElementById('config-section');
  const expanded = btn.getAttribute('aria-expanded') === 'true';
  btn.setAttribute('aria-expanded', String(!expanded));
  section.style.display = expanded ? 'none' : '';
});

// ─── Dashboard database reset ─────────────────────────────────────────────────
// Offered whenever the dashboard isn't running (e.g. to clear a stale cached
// identity or recover from a Postgres password mismatch). Data-only: it deletes
// the Postgres volume + indexer data and does NOT start anything. A missing
// volume is treated as success by the backend. On success we send the user back
// to the Status Monitor tab; on failure we stay and show the error.
document.getElementById('dashboard-reset-btn')?.addEventListener('click', async () => {
  const btn = document.getElementById('dashboard-reset-btn');
  const msg = document.getElementById('dashboard-empty-msg');
  if (btn) btn.disabled = true;
  if (msg) msg.textContent = 'Resetting dashboard database…';
  try {
    await invoke('reset_dashboard_database');
    state.dashboardDbError = null;
    await pollStatus();
    activateTab('status');
  } catch (e) {
    state.dashboardDbError = `Reset failed: ${e}`;
    appendLog({ timestamp: '', level: 'ERROR', message: state.dashboardDbError });
    refreshDashboardTab();
  } finally {
    if (btn) btn.disabled = false;
  }
});

// ─── Log drawer toggle ──────────────────────────────────────────────────────
document.getElementById('log-drawer-handle').addEventListener('click', (e) => {
  // Don't toggle if clicking Copy/Clear buttons inside the handle
  if (e.target.closest('.btn')) return;
  document.getElementById('log-drawer').classList.toggle('expanded');
});

// ─── Requirements toggle ─────────────────────────────────────────────────────
document.getElementById('checklist-toggle').addEventListener('click', () => {
  const btn = document.getElementById('checklist-toggle');
  const list = document.getElementById('checklist');
  const expanded = btn.getAttribute('aria-expanded') === 'true';
  btn.setAttribute('aria-expanded', String(!expanded));
  list.style.display = expanded ? 'none' : '';
});

// ─── Port change → re-run port-related checks ────────────────────────────────
document.getElementById('port').addEventListener('change', async () => {
  const port = parseInt(document.getElementById('port').value) || 20049;
  if (state.settings) {
    state.settings.node_config.port = port;
    await invoke('update_settings', { settings: state.settings }).catch(console.error);
    await invoke('recheck', { ids: ['port'] }).catch(console.error);
  }
});

document.getElementById('validator-port').addEventListener('change', async () => {
  const port = parseInt(document.getElementById('validator-port').value) || 30333;
  if (state.settings) {
    state.settings.node_config.validator_port = port;
    await invoke('update_settings', { settings: state.settings }).catch(console.error);
    await invoke('recheck', { ids: ['port-validator'] }).catch(console.error);
  }
});

// ─── Custom settings toggle ───────────────────────────────────────────────────
document.getElementById('btn-custom-toggle').addEventListener('click', () => {
  const btn = document.getElementById('btn-custom-toggle');
  const section = document.getElementById('custom-settings-section');
  const expanded = btn.getAttribute('aria-expanded') === 'true';
  btn.setAttribute('aria-expanded', String(!expanded));
  section.style.display = expanded ? 'none' : '';
});

// ─── Storage directory ───────────────────────────────────────────────────────
document.getElementById('data-dir').addEventListener('input', () => {
  const current = state._currentDataDir || '';
  const val = document.getElementById('data-dir').value.trim();
  const btn = document.getElementById('btn-data-dir-restart');
  btn.style.display = val !== current ? '' : 'none';
});

document.getElementById('btn-data-dir-restart').addEventListener('click', async () => {
  const val = document.getElementById('data-dir').value.trim();
  const btn = document.getElementById('btn-data-dir-restart');
  btn.disabled = true;
  btn.textContent = 'Saving\u2026';
  try {
    await invoke('set_data_dir', { path: val });
    await invoke('restart_app');
  } catch (e) {
    appendLog({ timestamp: '', level: 'ERROR', message: `Failed to set storage dir: ${e}` });
    btn.disabled = false;
    btn.textContent = 'Save & Restart';
  }
});

// ─── Run mode ─────────────────────────────────────────────────────────
//
// run_mode has no dedicated UI control. On macOS it is driven by the Metal
// GPU toggle in renderGpuDevices (on -> native/Metal, off -> docker/CPU);
// everywhere else the manager always runs the Dockerized stack.

/// Flip run_mode from the Metal toggle, persist, and re-run the checklist
/// (the visible checks differ between native and docker — e.g. the native
/// binary check).
async function setMetalEnabled(enabled) {
  if (!state.settings) return;
  state.settings.run_mode = enabled ? 'native' : 'docker';
  await invoke('update_settings', { settings: state.settings }).catch(console.error);
  // Mode change invalidates the whole cache — backend reseeds and reruns.
  state.checks.clear();
  await invoke('recheck').catch(console.error);
  updateRunModeUI();
}

function updateRunModeUI() {
  const isMac = state.hardwareSurvey?.os === 'macos';

  // Off macOS there is no Metal backend, so the manager always runs the
  // Dockerized stack. On macOS run_mode reflects the Metal toggle.
  if (!isMac && state.settings) {
    state.settings.run_mode = 'docker';
  }

  // Checklist items are filtered by mode inside renderChecklist(); re-render
  // here because mode and the hardware survey (WSL visibility) can land
  // in either order during init.
  renderChecklist();
  renderGpuDevices();
}

function renderGpuDevices() {
  const list = document.getElementById('gpu-device-list');
  const noDevices = document.getElementById('gpu-no-devices');
  const globalSettings = document.getElementById('gpu-global-settings');
  const survey = state.hardwareSurvey;
  const devices = survey?.gpu_devices || [];
  const isMetal = survey?.gpu_backend === 'metal';
  // On macOS the Metal toggle IS the run-mode selector: native = Metal on.
  const metalEnabled = (state.settings?.run_mode || 'docker') === 'native';

  // Metal exposes adaptive-cap knobs, but only while it's enabled; CUDA never.
  const metalExtra = document.getElementById('metal-extra-settings');
  if (metalExtra) {
    metalExtra.style.display = isMetal && metalEnabled ? '' : 'none';
  }

  list.replaceChildren();

  if (devices.length === 0) {
    noDevices.style.display = '';
    globalSettings.style.opacity = '0.4';
    globalSettings.style.pointerEvents = 'none';
    return;
  }

  noDevices.style.display = 'none';
  globalSettings.style.opacity = '';
  globalSettings.style.pointerEvents = '';

  const savedConfigs = state.settings?.node_config?.gpu_device_configs || [];

  devices.forEach((dev) => {
    const saved = savedConfigs.find((c) => c.index === dev.index);
    const enabled = saved ? saved.enabled : false;
    const mem = dev.memory_mb ? ` (${dev.memory_mb} MB)` : '';
    const backendLabel = isMetal ? 'Metal' : 'CUDA';

    const row = document.createElement('div');
    row.style.cssText = 'display:flex;align-items:center;gap:10px;padding:6px 0;';

    const label = document.createElement('label');
    label.className = 'gpu-toggle-switch';
    const checkbox = document.createElement('input');
    checkbox.type = 'checkbox';
    const slider = document.createElement('span');
    slider.className = 'gpu-toggle-slider';

    if (isMetal) {
      // Metal is a single implicit GPU only reachable from the native miner,
      // so its enable toggle doubles as the run-mode selector: on → native
      // (Metal mining), off → docker (CPU mining).
      checkbox.className = 'metal-enable-toggle';
      checkbox.checked = metalEnabled;
      checkbox.addEventListener('change', () => setMetalEnabled(checkbox.checked));
    } else {
      // CUDA gets a per-device toggle so individual cards can be enabled.
      checkbox.className = 'gpu-device-toggle';
      checkbox.dataset.index = String(dev.index);
      checkbox.checked = enabled;
    }
    label.appendChild(checkbox);
    label.appendChild(slider);
    row.appendChild(label);

    const text = document.createElement('span');
    text.style.fontSize = '13px';
    text.textContent = `GPU ${dev.index}: ${dev.name} (${backendLabel})${mem}`;

    row.appendChild(text);
    list.appendChild(row);
  });

  // Make the Metal on/off → mining-mode consequence explicit.
  if (isMetal) {
    const hint = document.createElement('div');
    hint.style.cssText = 'font-size:12px;color:var(--text-faint);padding:2px 0 0 0;';
    hint.textContent = metalEnabled
      ? 'Metal GPU mining enabled — runs the native miner.'
      : 'Metal off — the node runs CPU mining via Docker.';
    list.appendChild(hint);
  }
}

// ─── TLS guide toggle ────────────────────────────────────────────────────────
document.getElementById('btn-tls-guide-toggle')?.addEventListener('click', () => {
  const btn = document.getElementById('btn-tls-guide-toggle');
  const guide = document.getElementById('tls-guide');
  const expanded = btn.getAttribute('aria-expanded') === 'true';
  btn.setAttribute('aria-expanded', String(!expanded));
  guide.style.display = expanded ? 'none' : '';
});

// ─── Secret show/hide & regenerate ───────────────────────────────────────────
document.getElementById('btn-show-secret').addEventListener('click', () => {
  const input = document.getElementById('secret-display');
  const btn = document.getElementById('btn-show-secret');
  if (input.type === 'password') {
    input.type = 'text';
    btn.textContent = 'Hide';
  } else {
    input.type = 'password';
    btn.textContent = 'Show';
  }
});

document.getElementById('btn-regen-secret').addEventListener('click', async () => {
  try {
    const secret = await invoke('generate_node_secret');
    document.getElementById('secret-display').value = secret;
    if (state.settings) {
      state.settings.node_config.secret = secret;
      await invoke('update_settings', { settings: state.settings });
    }
    await invoke('recheck', { ids: ['secret'] }).catch(console.error);
  } catch (e) {
    console.error('Failed to regenerate secret:', e);
  }
});

// ─── Public host enable toggle ────────────────────────────────────────────────
document.getElementById('public-host-enable').addEventListener('change', () => {
  const enabled = document.getElementById('public-host-enable').checked;
  document.getElementById('public-host').disabled = !enabled;
  document.getElementById('public-port').disabled = !enabled;
  if (!enabled) {
    document.getElementById('public-host').value = '';
    document.getElementById('public-port').value = '';
  }
});

// ─── D-Wave section toggle ───────────────────────────────────────────────────
document.getElementById('btn-qpu-toggle').addEventListener('click', () => {
  const section = document.getElementById('qpu-section');
  const btn = document.getElementById('btn-qpu-toggle');
  const isVisible = section.style.display !== 'none';
  section.style.display = isVisible ? 'none' : 'block';
  btn.textContent = isVisible
    ? 'Configure D-Wave Access'
    : 'Hide D-Wave Configuration';
});

// ─── GPU utilization slider ──────────────────────────────────────────────────
document.getElementById('gpu-utilization').addEventListener('input', () => {
  const val = document.getElementById('gpu-utilization').value;
  document.getElementById('gpu-util-display').textContent = `${val}%`;
});

// ─── Metal active-utilization slider ─────────────────────────────────────────
document.getElementById('metal-active-util').addEventListener('input', () => {
  const val = document.getElementById('metal-active-util').value;
  document.getElementById('metal-active-util-display').textContent = `${val}%`;
});

// ─── Collect form → NodeConfig ────────────────────────────────────────────────
function collectConfig() {
  const gpuUtilization = parseInt(document.getElementById('gpu-utilization')?.value) || 80;
  const gpuYielding = document.getElementById('gpu-yielding')?.checked ?? false;
  const survey = state.hardwareSurvey;
  const gpuBackend = survey?.gpu_backend === 'metal' ? 'mps' : 'local';

  // Build per-device configs from toggle checkboxes
  const gpuDeviceConfigs = [];
  document.querySelectorAll('.gpu-device-toggle').forEach((cb) => {
    gpuDeviceConfigs.push({
      index: parseInt(cb.dataset.index),
      enabled: cb.checked,
      utilization: gpuUtilization,
      yielding: gpuYielding,
    });
  });

  // Metal tuning is a standalone single-GPU config (the [metal] section);
  // utilization/yielding are shared with the slider above, active_util and
  // idle_after_s are Metal-only adaptive-cap knobs.
  const metalConfig = {
    utilization: gpuUtilization,
    yielding: gpuYielding,
    active_util: parseInt(document.getElementById('metal-active-util')?.value) || 85,
    idle_after_s: parseInt(document.getElementById('metal-idle-after')?.value) || 60,
  };

  const qpuToken = document.getElementById('qpu-api-key')?.value?.trim() ?? '';
  const dwaveConfig = qpuToken
    ? {
        token: qpuToken,
        solver: 'Advantage2_System1.13',
        dwave_region_url: 'https://na-west-1.cloud.dwavesys.com/sapi/v2/',
        daily_budget: document.getElementById('qpu-daily-budget')?.value?.trim() ?? '',
        qpu_min_blocks_for_estimation: null,
        qpu_ema_alpha: null,
      }
    : null;

  const base = state.settings?.node_config ?? {};

  return {
    port: parseInt(document.getElementById('port').value) || 20049,
    validator_port: parseInt(document.getElementById('validator-port').value) || 30333,
    validator_rpc_port: parseInt(document.getElementById('validator-rpc-port')?.value) || 9944,
    listen: base.listen ?? '::',
    public_host: document.getElementById('public-host-enable')?.checked
      ? document.getElementById('public-host')?.value?.trim() ?? ''
      : '',
    public_port: document.getElementById('public-host-enable')?.checked
      ? (parseInt(document.getElementById('public-port')?.value) || null)
      : null,
    node_name: document.getElementById('node-name')?.value?.trim() ?? '',
    peers: base.peers ?? [],
    auto_mine: base.auto_mine ?? false,
    secret: state.settings?.node_config?.secret ?? '',
    genesis_config: base.genesis_config ?? 'genesis_block.json',
    tofu: base.tofu ?? true,
    trust_db: base.trust_db ?? '~/.quip/trust.db',
    tls_cert_file: base.tls_cert_file ?? '',
    tls_key_file: base.tls_key_file ?? '',
    verify_tls: base.verify_tls ?? false,
    rest_host: base.rest_host ?? '127.0.0.1',
    rest_port: base.rest_port ?? -1,
    rest_insecure_port: base.rest_insecure_port ?? -1,
    telemetry_enabled: base.telemetry_enabled ?? true,
    telemetry_dir: base.telemetry_dir ?? 'telemetry',
    log_level: document.getElementById('log-level')?.value || 'info',
    node_log: document.getElementById('node-log')?.value?.trim() ?? '',
    http_log: document.getElementById('http-log')?.value?.trim() ?? '',
    cpu_enabled: document.getElementById('cpu-enabled')?.checked ?? true,
    num_cpus: parseInt(document.getElementById('num-cpus').value) || 1,
    gpu_backend: gpuBackend,
    gpu_device_configs: gpuDeviceConfigs,
    metal_config: metalConfig,
    dwave_config: dwaveConfig,
    timeout: base.timeout ?? 3,
    heartbeat_interval: base.heartbeat_interval ?? 15,
    heartbeat_timeout: base.heartbeat_timeout ?? 300,
    fanout: base.fanout ?? null,
  };
}

// ─── Apply form → settings ────────────────────────────────────────────────────
function applyFormToSettings() {
  if (!state.settings) return;
  state.settings.node_config = collectConfig();

  // Image is auto-derived from the GPU config: CUDA when any NVIDIA GPU is
  // enabled, CPU otherwise. D-Wave mining is a config.toml [dwave] concern,
  // not a separate image.
  const hasEnabledCuda = (state.settings.node_config.gpu_device_configs || [])
    .some((d) => d.enabled) && state.hardwareSurvey?.gpu_backend === 'cuda';
  state.settings.image_tag = hasEnabledCuda ? 'cuda' : 'cpu';

  state.settings.tls_enabled =
    document.getElementById('tls-enabled')?.checked ?? false;
  state.settings.hostname =
    document.getElementById('hostname')?.value?.trim() ||
    DEFAULT_DASHBOARD_HOSTNAME;
  state.settings.cert_email =
    document.getElementById('cert-email')?.value?.trim() || '';
  state.settings.zerossl_api_key =
    document.getElementById('zerossl-api-key')?.value ?? '';
}

// ─── Populate form from settings ─────────────────────────────────────────────
function populateForm(settings) {
  const c = settings.node_config;

  // Validator / miner configuration
  document.getElementById('port').value = c.port ?? 20049;
  document.getElementById('validator-port').value = c.validator_port ?? 30333;
  document.getElementById('validator-rpc-port').value = c.validator_rpc_port ?? 9944;
  document.getElementById('secret-display').value = c.secret ?? '';

  // Custom settings
  document.getElementById('node-name').value = c.node_name ?? '';
  const publicHost = c.public_host ?? '';
  const publicPort = c.public_port ?? null;
  const publicOverrideEnabled = !!(publicHost || publicPort);
  document.getElementById('public-host-enable').checked = publicOverrideEnabled;
  document.getElementById('public-host').disabled = !publicOverrideEnabled;
  document.getElementById('public-port').disabled = !publicOverrideEnabled;
  document.getElementById('public-host').value = publicHost;
  document.getElementById('public-port').value = publicPort ?? '';
  document.getElementById('log-level').value = c.log_level ?? 'info';
  document.getElementById('node-log').value = c.node_log ?? '';
  document.getElementById('http-log').value = c.http_log ?? '';

  // Stack Configuration (image_tag is auto-derived, no UI control)
  settings.update_channel = settings.update_channel ?? 'release';
  updateChannelButtons();
  refreshChannelInfo();
  document.getElementById('tls-enabled').checked =
    settings.tls_enabled ?? false;
  document.getElementById('hostname').value =
    settings.hostname ?? DEFAULT_DASHBOARD_HOSTNAME;
  document.getElementById('cert-email').value = settings.cert_email ?? '';
  document.getElementById('zerossl-api-key').value =
    settings.zerossl_api_key ?? '';
  updateStackUiVisibility();

  // Auto-expand custom settings if any non-default values are set
  const hasCustom =
    publicHost || publicPort ||
    c.log_level !== 'info' ||
    (c.node_log ?? '') ||
    (c.http_log ?? '');
  if (hasCustom) {
    document.getElementById('btn-custom-toggle').setAttribute('aria-expanded', 'true');
    document.getElementById('custom-settings-section').style.display = '';
  }

  // CPU Miner
  document.getElementById('cpu-enabled').checked = c.cpu_enabled ?? true;
  document.getElementById('num-cpus').value = c.num_cpus ?? 1;
  updateCpuUiVisibility();

  // GPU Miner — for Metal, utilization/yielding come from metal_config;
  // for CUDA, from the first enabled device (or defaults).
  const isMetal = state.hardwareSurvey?.gpu_backend === 'metal';
  const metalCfg = c.metal_config ?? {};
  const gpuCfg = (c.gpu_device_configs || []).find((d) => d.enabled) || (c.gpu_device_configs || [])[0];
  const savedUtil = isMetal ? (metalCfg.utilization ?? 100) : (gpuCfg?.utilization ?? 80);
  document.getElementById('gpu-utilization').value = savedUtil;
  document.getElementById('gpu-util-display').textContent = `${savedUtil}%`;
  document.getElementById('gpu-yielding').checked = isMetal
    ? (metalCfg.yielding ?? true)
    : (gpuCfg?.yielding ?? false);

  // Metal-only adaptive-cap knobs
  const activeUtil = metalCfg.active_util ?? 85;
  document.getElementById('metal-active-util').value = activeUtil;
  document.getElementById('metal-active-util-display').textContent = `${activeUtil}%`;
  document.getElementById('metal-idle-after').value = metalCfg.idle_after_s ?? 60;

  // D-Wave
  const dw = c.dwave_config;
  if (dw) {
    document.getElementById('qpu-api-key').value = dw.token ?? '';
    document.getElementById('qpu-daily-budget').value = dw.daily_budget ?? '';
    if (dw.token) {
      document.getElementById('qpu-section').style.display = 'block';
      document.getElementById('btn-qpu-toggle').textContent =
        'Hide D-Wave Configuration';
    }
  }
  // GPU device list rendered after list_gpu_devices call in init
}

// ─── Start/Stop/Apply enable state ───────────────────────────────────────────
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

// ─── Status circle ────────────────────────────────────────────────────────────
function setStatus(stateStr) {
  const dot = document.getElementById('status-dot');
  const text = document.getElementById('status-text');
  const sub = document.getElementById('status-subtext');

  dot.className = 'status-dot';
  text.className = 'status-text';

  if (stateStr === 'running') {
    dot.classList.add('status-running', 'active');
    text.classList.add('status-running');
    text.textContent = 'RUNNING';
    sub.textContent = 'Node is running';
  } else if (stateStr === 'degraded') {
    dot.classList.add('status-degraded', 'active');
    text.classList.add('status-degraded');
    text.textContent = 'DEGRADED';
    sub.textContent = 'Running, but some stack services are down or unhealthy';
  } else if (stateStr === 'unhealthy') {
    dot.classList.add('status-unhealthy', 'active');
    text.classList.add('status-unhealthy');
    text.textContent = 'UNHEALTHY';
    sub.textContent = 'Node health checks failing';
  } else {
    dot.classList.add('status-stopped');
    text.classList.add('status-stopped');
    text.textContent = 'STOPPED';
    sub.textContent = 'Node not running';
  }
}

// ─── Checklist render (FSM) ──────────────────────────────────────────────────
//
// The backend emits one CheckItem per `checklist-update` event. We merge
// by id into state.checks and repaint. No per-item listener churn:
// everything routes through one delegated click handler at the bottom
// of this section.

// Mirror of checklist.rs::visible_for_mode — which ids render for the
// current settings. Backend already filters this way; duplicating the
// logic here prevents the frontend from ever drawing a placeholder for
// a check that can't apply to the current profile.
function visibleInMode(id, runMode) {
  const s = state.settings;
  const isDocker = (runMode || 'docker') === 'docker';
  const hasDwave = !!s?.node_config?.dwave_config;
  // Compose runs in both Docker mode and Native mode. Native uses it for the
  // validator/dashboard support services even when the miner runs on the host.
  const composeWillRun = true;

  switch (id) {
    case 'docker':
    case 'docker-compose':
      return composeWillRun;
    case 'wsl':
      return isDocker && state.hardwareSurvey?.os === 'windows';
    case 'binary':
      return !isDocker;
    case 'dwave-key':
      return hasDwave;
    // version / secret / ip / hostname / ports — always shown.
    default:
      return true;
  }
}

function renderChecklistItem(item) {
  const li = document.createElement('li');
  li.className = 'checklist-item';
  li.dataset.id = item.id;

  const icon = document.createElement('span');
  icon.className = `check-icon state-${item.state}`;
  icon.textContent = STATE_ICON[item.state] || STATE_ICON.idle;

  const label = document.createElement('span');
  label.className = 'check-label';
  label.textContent = item.label;
  if (item.detail) label.title = item.detail;

  const actions = document.createElement('div');
  actions.className = 'check-actions';

  const recheckBtn = document.createElement('button');
  recheckBtn.type = 'button';
  recheckBtn.className = 'btn btn-sm btn-secondary check-action';
  recheckBtn.dataset.action = 'recheck';
  recheckBtn.textContent = item.state === 'running' ? 'Checking…' : 'Retry';
  recheckBtn.disabled = item.state === 'running';
  actions.appendChild(recheckBtn);

  // For checks whose fix is folded into Retry (Retry runs the fix, then
  // rechecks) we drop the dedicated fix button entirely.
  if (
    item.fixable &&
    !FIX_FOLDED_INTO_RETRY.has(item.id) &&
    (item.state === 'fail' || item.state === 'warn')
  ) {
    const fixBtn = document.createElement('button');
    fixBtn.type = 'button';
    fixBtn.className = 'btn btn-sm btn-secondary check-action';
    fixBtn.dataset.action = 'fix';
    fixBtn.textContent = FIX_LABELS[item.fixable.kind] || 'Fix';
    actions.appendChild(fixBtn);
  }

  li.append(icon, label, actions);
  return li;
}

function renderChecklist() {
  const runMode = state.settings?.run_mode || 'docker';
  const ul = document.getElementById('checklist');
  const visible = CHECK_ORDER.filter((id) => visibleInMode(id, runMode));

  const rows = visible.map((id) => {
    const item = state.checks.get(id) || {
      id, state: 'idle', label: defaultLabel(id), required: false, fixable: null,
    };
    return renderChecklistItem(item);
  });
  ul.replaceChildren(...rows);

  updateChecklistSummary(visible);
}

function defaultLabel(id) {
  const port = state.settings?.node_config?.port ?? 20049;
  const validatorPort = state.settings?.node_config?.validator_port ?? 30333;
  switch (id) {
    case 'docker':            return 'Docker installed & running';
    case 'docker-compose':    return 'Docker Compose v2 available';
    case 'wsl':               return 'WSL installed with distro';
    case 'binary':            return 'Native miner binary available';
    case 'secret':            return 'Node secret configured';
    case 'ip':                return 'Public IP reachable';
    case 'hostname':          return 'Hostname accessible to internet';
    case 'port':              return `Public API port ${port} — press Retry to test`;
    case 'port-validator':    return `Validator P2P port ${validatorPort} reachable`;
    case 'dwave-key':         return 'D-Wave API token configured';
    default:                  return id;
  }
}

function updateChecklistSummary(visibleIds) {
  const items = visibleIds.map((id) => state.checks.get(id)).filter(Boolean);
  const allRun = items.length === visibleIds.length &&
                 items.every((i) => i.state !== 'idle' && i.state !== 'running');

  const requiredFailing = items.filter(
    (i) => i.required === true && i.state === 'fail'
  ).length;
  const warnings = items.filter(
    (i) => i.state === 'warn' || (i.required !== true && i.state === 'fail')
  ).length;

  state.checksPassed = allRun && requiredFailing === 0;

  const summary = document.getElementById('checklist-summary');
  const checklistEl = document.getElementById('checklist');
  const toggleBtn = document.getElementById('checklist-toggle');
  if (!summary || !toggleBtn) return;

  if (!allRun) {
    summary.textContent = 'Checking…';
    summary.style.color = 'var(--text-faint)';
  } else if (requiredFailing === 0 && warnings === 0) {
    summary.textContent = '✓ All requirements met';
    summary.style.color = 'var(--success)';
    toggleBtn.setAttribute('aria-expanded', 'false');
    checklistEl.style.display = 'none';
  } else if (requiredFailing === 0) {
    const s = warnings > 1 ? 's' : '';
    summary.textContent = `✓ Ready (${warnings} warning${s})`;
    summary.style.color = 'var(--warning)';
    toggleBtn.setAttribute('aria-expanded', 'false');
    checklistEl.style.display = 'none';
  } else {
    summary.textContent = `✗ ${requiredFailing} not met`;
    summary.style.color = 'var(--error)';
    toggleBtn.setAttribute('aria-expanded', 'true');
    checklistEl.style.display = '';
  }

  updateStartStopState();
}

function mergeCheckUpdate(item) {
  state.checks.set(item.id, item);
  renderChecklist();

  // The binary (Native) check covers miner freshness too. When it reaches a
  // terminal state the node version may have changed (a download update fix
  // ran), so refresh the header's "v<app> (node <node>)" label.
  if (
    item.id === 'binary' &&
    item.state !== 'idle' &&
    item.state !== 'running'
  ) {
    refreshNodeVersion();
  }
}

// ─── Fix action dispatcher ──────────────────────────────────────────────────
async function runFix(id) {
  const item = state.checks.get(id);
  if (!item || !item.fixable) return;

  const fix = item.fixable;
  switch (fix.kind) {
    case 'InstallDocker':
      openUrl('https://docs.docker.com/get-docker/');
      return;

    case 'GenerateSecret':
      try {
        const secret = await invoke('generate_node_secret');
        document.getElementById('secret-display').value = secret;
        if (state.settings) {
          state.settings.node_config.secret = secret;
          await invoke('update_settings', { settings: state.settings });
        }
        await invoke('recheck', { ids: ['secret'] }).catch(console.error);
      } catch (e) {
        console.error('Failed to generate secret:', e);
      }
      return;
  }
}

// ─── Checklist event delegation ──────────────────────────────────────────────
document.getElementById('checklist').addEventListener('click', (e) => {
  const btn = e.target.closest('button[data-action]');
  if (!btn) return;
  const li = btn.closest('.checklist-item');
  const id = li?.dataset.id;
  if (!id) return;
  if (btn.dataset.action === 'recheck') {
    const item = state.checks.get(id);
    const failing = item && (item.state === 'fail' || item.state === 'warn');
    if (FIX_FOLDED_INTO_RETRY.has(id) && failing) {
      // secret: Retry generates the secret, then rechecks — but only while
      // failing, so a passing check isn't reset. Downloads (stack images,
      // native binary) are pulled by the backend check itself on recheck.
      retryWithFix(id);
    } else {
      invoke('recheck', { ids: [id] }).catch(console.error);
    }
  } else if (btn.dataset.action === 'fix') {
    runFix(id);
  }
});

// Run a check's fix (generate secret), then recheck it.
async function retryWithFix(id) {
  await runFix(id);
  await invoke('recheck', { ids: [id] }).catch(console.error);
}

// ─── Global Retry All ─────────────────────────────────────────────────────────
document.getElementById('btn-recheck-all').addEventListener('click', () => {
  invoke('recheck').catch(console.error);
});

// ─── Log panel ────────────────────────────────────────────────────────────────
// Append `text` to `el`, turning http(s) URLs into clickable anchors (opened
// via the external-link handler at the bottom of this file). Built from DOM
// nodes, never innerHTML — log text streams from node output and must not be
// interpreted as markup.
function appendTextWithLinks(el, text) {
  const urlRe = /https?:\/\/\S+/g;
  let last = 0;
  for (const match of text.matchAll(urlRe)) {
    // Trailing punctuation is sentence structure, not part of the URL.
    const url = match[0].replace(/[.,;:!?)\]'"]+$/, '');
    if (match.index > last) {
      el.appendChild(document.createTextNode(text.slice(last, match.index)));
    }
    const a = document.createElement('a');
    a.href = url;
    a.textContent = url;
    el.appendChild(a);
    last = match.index + url.length;
  }
  if (last < text.length) {
    el.appendChild(document.createTextNode(text.slice(last)));
  }
}

function appendLog(entry) {
  state.logLines.push(entry);
  if (state.logLines.length > state.MAX_LOG_LINES) {
    state.logLines.shift();
  }
  const output = document.getElementById('log-output');
  const line = document.createElement('p');
  line.className = `log-line log-${(entry.level || 'info').toLowerCase()}`;
  const ts = entry.timestamp ? `[${entry.timestamp}] ` : '';
  appendTextWithLinks(line, `${ts}${entry.message}`);
  output.appendChild(line);
  if (output.scrollHeight - output.scrollTop - output.clientHeight < 60) {
    output.scrollTop = output.scrollHeight;
  }
  while (output.children.length > state.MAX_LOG_LINES) {
    output.removeChild(output.firstChild);
  }
}

document.getElementById('btn-copy-log').addEventListener('click', () => {
  const text = state.logLines
    .map((e) => `${e.timestamp} ${e.level} ${e.message}`)
    .join('\n');
  navigator.clipboard.writeText(text).catch(console.error);
});

document.getElementById('btn-clear-log').addEventListener('click', () => {
  state.logLines = [];
  document.getElementById('log-output').innerHTML = '';
});

// ─── Image pull progress ──────────────────────────────────────────────────────
// docker compose --progress json streams per-layer events; we aggregate them
// into one bar per image (summing the layer byte counts) and render them live
// in the log drawer, hiding the panel once every image reports done.
function friendlyImageName(id) {
  // "Image registry.gitlab.com/ns/quip-miner-cpu:v0.2" -> "quip-miner-cpu:v0.2"
  const ref = String(id).replace(/^Image\s+/, '');
  const slash = ref.lastIndexOf('/');
  return slash >= 0 ? ref.slice(slash + 1) : ref;
}

function handlePullProgress(ev) {
  // Legacy {line} events (non-pull compose commands) have no id — ignore them.
  if (!ev || typeof ev.id !== 'string') return;

  // Generation gating. Each pull is stamped with a monotonic `gen` by the
  // backend; `pull-complete` records it in pullCompletedGen. An event at or
  // below that watermark is a late straggler from a pull we've already closed
  // (event delivery across the backend's reader threads is not ordered), so
  // dropping it is what keeps the panel from being resurrected after it hides.
  const gen = Number(ev.gen) || 0;
  if (gen <= state.pullCompletedGen) return;

  // A higher generation (or no live session) starts a fresh panel; a lower one
  // belongs to a superseded pull and is ignored.
  if (!state.pull || !state.pull.active || gen > state.pull.gen) {
    state.pull = { active: true, gen, title: 'Pulling stack images', images: new Map() };
  } else if (gen < state.pull.gen) {
    return;
  }

  const isImageLevel = !ev.parent_id && ev.id.startsWith('Image ');
  const imageId = isImageLevel ? ev.id : ev.parent_id || ev.id;
  let img = state.pull.images.get(imageId);
  if (!img) {
    img = { name: friendlyImageName(imageId), layers: new Map(), done: false };
    state.pull.images.set(imageId, img);
  }

  if (isImageLevel) {
    if (ev.status === 'Done' || ev.text === 'Pulled') img.done = true;
  } else {
    // The presence of any layer event is what distinguishes an image that is
    // actually downloading from one that was already up to date — the latter
    // emits only image-level Working/Done events and no layers, so it never
    // gets a row (see renderPullPanel).
    const layerDone = ev.text === 'Pull complete' || ev.text === 'Already exists';
    const prev = img.layers.get(ev.id) || { cur: 0, tot: 0 };
    const tot = Number(ev.total) || prev.tot;
    img.layers.set(ev.id, {
      cur: layerDone && tot ? tot : Math.max(prev.cur, Number(ev.current) || 0),
      tot,
    });
  }

  renderPullPanel();
  // The panel is closed authoritatively by `pull-complete` (generation-keyed),
  // not by counting per-image "Pulled" events — that accounting could miss an
  // image's terminal event and, after closing, be reopened by a straggler.
}

function renderPullPanel() {
  const panel = document.getElementById('pull-progress-panel');
  if (!panel || !state.pull) return;

  // Only images that produced layer events are actually being downloaded;
  // already-up-to-date images emit only image-level Working/Done events and are
  // omitted. If nothing is downloading, the panel stays hidden entirely rather
  // than flashing a bare title.
  const downloading = [...state.pull.images.values()].filter((img) => img.layers.size > 0);
  if (downloading.length === 0) {
    panel.style.display = 'none';
    panel.replaceChildren();
    return;
  }
  panel.style.display = '';

  const title = document.createElement('div');
  title.className = 'pull-progress-title';
  title.textContent = state.pull.title || 'Downloading';
  const rows = [title];

  for (const img of downloading) {
    const layers = [...img.layers.values()];
    const tot = layers.reduce((s, l) => s + l.tot, 0);
    const cur = layers.reduce((s, l) => s + l.cur, 0);
    const pct = img.done ? 100 : tot > 0 ? Math.min(100, Math.round((cur / tot) * 100)) : 0;

    const row = document.createElement('div');
    row.className = 'pp-row';

    const name = document.createElement('span');
    name.className = 'pp-name';
    name.textContent = img.name;

    const bar = document.createElement('span');
    bar.className = 'pp-bar';
    const fill = document.createElement('span');
    fill.className = `pp-bar-fill${img.done ? ' done' : ''}`;
    fill.style.width = `${pct}%`;
    bar.appendChild(fill);

    const detail = document.createElement('span');
    detail.className = 'pp-detail';
    detail.textContent = img.done
      ? 'done'
      : tot > 0
        ? `${toMB(cur)}/${toMB(tot)} MB`
        : `${pct}%`;

    row.append(name, bar, detail);
    rows.push(row);
  }
  panel.replaceChildren(...rows);
}

function toMB(bytes) {
  return (bytes / 1_048_576).toFixed(0);
}

function finishPullProgress() {
  if (state._pullHideTimer) {
    clearTimeout(state._pullHideTimer);
    state._pullHideTimer = null;
  }
  const panel = document.getElementById('pull-progress-panel');
  if (panel) {
    panel.style.display = 'none';
    panel.replaceChildren();
  }
  state.pull = null;
}

// The native miner binary download reuses the same panel as a single bar.
function handleBinaryDownloadProgress(ev) {
  const { downloaded, total, done } = ev || {};
  if (!state.pull || !state.pull.active) {
    // gen: Infinity keeps a late docker `pull-complete` (always a finite
    // generation) from closing this native-miner download, which shares the
    // panel but isn't part of the docker pull generation sequence.
    state.pull = { active: true, gen: Infinity, title: 'Downloading native miner', images: new Map() };
  }
  let img = state.pull.images.get('native-miner');
  if (!img) {
    img = { name: 'native miner', layers: new Map(), done: false };
    state.pull.images.set('native-miner', img);
  }
  img.layers.set('binary', { cur: Number(downloaded) || 0, tot: Number(total) || 0 });
  if (done) img.done = true;
  renderPullPanel();
  if (done) {
    if (state._pullHideTimer) clearTimeout(state._pullHideTimer);
    state._pullHideTimer = setTimeout(finishPullProgress, 1500);
  }
}

// ─── Helpers for run-mode dispatch ────────────────────────────────────────────
function isDockerMode() {
  return (state.settings?.run_mode ?? 'docker') === 'docker';
}

function collapseConfig() {
  document.getElementById('btn-config-toggle').setAttribute('aria-expanded', 'false');
  document.getElementById('config-section').style.display = 'none';
}

function expandConfig() {
  document.getElementById('btn-config-toggle').setAttribute('aria-expanded', 'true');
  document.getElementById('config-section').style.display = '';
}

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

async function startNode() {
  if (isDockerMode()) {
    await invoke('start_stack');
    await invoke('start_log_stream');
  } else {
    // Native mode: run the binary on the host + the compose stack's
    // non-node services so the user still gets the dashboard UI.
    await invoke('start_stack');
    await invoke('start_native_node');
  }
  collapseConfig();
}

async function stopNode() {
  if (isDockerMode()) {
    await invoke('stop_log_stream');
    await invoke('stop_stack');
  } else {
    await invoke('stop_native_node');
    try { await invoke('stop_stack'); } catch (e) { console.error('stack stop:', e); }
  }
}

// ─── Start / Stop ─────────────────────────────────────────────────────────────
document.getElementById('btn-start').addEventListener('click', async () => {
  if (state.updateAvailable) {
    await runRestartToUpdate();
    return;
  }
  const applyStatus = document.getElementById('apply-status');
  state.starting = true;
  updateStartStopState();
  applyStatus.textContent = 'Starting\u2026';
  appendLog({ timestamp: '', level: 'INFO', message: 'Starting node manager stack…' });
  try {
    if (!state.settings) throw new Error('settings are not loaded yet');
    applyFormToSettings();
    await invoke('update_settings', { settings: state.settings });
    await startNode();
    applyStatus.textContent = 'Node started.';
    await pollStatus();
  } catch (e) {
    applyStatus.textContent = `Error: ${e}`;
    appendLog({ timestamp: '', level: 'ERROR', message: `Start failed: ${e}` });
  } finally {
    state.starting = false;
    updateStartStopState();
  }
});

document.getElementById('btn-stop').addEventListener('click', async () => {
  const applyStatus = document.getElementById('apply-status');
  state.stopping = true;
  updateStartStopState();
  applyStatus.textContent = 'Stopping\u2026';
  try {
    await stopNode();
    state.containerRunning = false;
    state.nativeRunning = false;
    state.health = null;
    setStatus('stopped');
    updateStartStopState();
    expandConfig();
    applyStatus.textContent = 'Node stopped.';
  } catch (e) {
    applyStatus.textContent = `Error: ${e}`;
    appendLog({ timestamp: '', level: 'ERROR', message: `Stop failed: ${e}` });
  } finally {
    state.stopping = false;
    updateStartStopState();
  }
});

// ─── Apply & Restart ──────────────────────────────────────────────────────────
document.getElementById('btn-apply').addEventListener('click', async () => {
  const applyStatus = document.getElementById('apply-status');
  state.starting = true;
  updateStartStopState();
  applyStatus.textContent = 'Applying\u2026';
  try {
    if (!state.settings) throw new Error('settings are not loaded yet');
    applyFormToSettings();
    await invoke('update_settings', { settings: state.settings });
    const running = isDockerMode() ? state.containerRunning : state.nativeRunning;
    if (running) {
      applyStatus.textContent = 'Restarting\u2026';
      await stopNode();
    }
    applyStatus.textContent = running ? 'Restarting\u2026' : 'Starting\u2026';
    await startNode();
    applyStatus.textContent = 'Node running.';
    await pollStatus();
    setTimeout(() => {
      applyStatus.textContent = '';
    }, 3000);
  } catch (e) {
    applyStatus.textContent = `Error: ${e}`;
    appendLog({ timestamp: '', level: 'ERROR', message: `Apply failed: ${e}` });
  } finally {
    state.starting = false;
    updateStartStopState();
  }
});

// ─── Save ─────────────────────────────────────────────────────────────────────
document.getElementById('btn-save').addEventListener('click', async () => {
  applyFormToSettings();
  const applyStatus = document.getElementById('apply-status');
  applyStatus.textContent = 'Saving\u2026';
  try {
    await invoke('update_settings', { settings: state.settings });
    applyStatus.textContent = 'Settings saved.';
    setTimeout(() => { applyStatus.textContent = ''; }, 3000);
  } catch (e) {
    applyStatus.textContent = `Error: ${e}`;
  }
});

// ─── Polling ──────────────────────────────────────────────────────────────────
// `containerRunning` now means "the compose stack's node service is up" in
// Docker mode, or "the compose stack has services up" as a proxy for "the
// manager is managing something" in Native mode.
function stackRunningInMode() {
  const s = state.stack;
  if (!s) return false;
  if (isDockerMode()) {
    // Node container must be one of the running services in Docker mode.
    const nodeRunning = s.services?.some(
      (x) => ['cpu', 'cuda'].includes(x.service) && x.running,
    );
    return !!nodeRunning;
  }
  return s.services?.some((x) => x.running);
}

// Map the stack roll-up + miner state to the status pill. The miner (container
// or native process) decides RUNNING vs STOPPED; support-service health
// decides RUNNING vs DEGRADED. In Native mode a null stack means the support
// containers aren't reachable at all — the miner alone is degraded, not fine.
function statusFromStack(minerRunning) {
  if (!minerRunning) return 'stopped';
  // Mid-Start/Stop the services settle one at a time; don't flash DEGRADED
  // while the transition is still in flight.
  if (state.starting || state.stopping) return 'running';
  // When the health monitor has reported, use its verdict directly so the
  // pill reflects the three-dimensional check (infra + chain + participation).
  if (state.health?.overall) return state.health.overall;
  const overall = state.stack?.overall ?? 'stopped';
  return overall === 'running' ? 'running' : 'degraded';
}

// Re-paint the status pill from current state without a full poll round-trip.
// Called by renderHealth() so an inbound health-changed event immediately
// updates the pill without waiting for the next 10 s pollStatus tick.
function refreshStatusPill() {
  const minerRunning = state.containerRunning || state.nativeRunning;
  setStatus(statusFromStack(minerRunning));
}

async function pollStatus() {
  try {
    // Stack status is valid in both Docker and Native modes (Native still
    // runs the validator/dashboard/postgres/caddy support services).
    try {
      state.stack = await invoke('get_stack_status');
    } catch {
      // Keep the last known stack. The backend errors deliberately when
      // `compose ps` output is unparseable, precisely so a running stack
      // isn't reported as stopped (which would re-enable Start).
    }

    if (isDockerMode()) {
      state.containerRunning = stackRunningInMode();
      state.nativeRunning = false;
      setStatus(statusFromStack(state.containerRunning));
    } else {
      const status = await invoke('get_native_node_status');
      state.nativeRunning = status.running;
      state.containerRunning = false;
      setStatus(statusFromStack(status.running));
    }
  } catch {
    state.containerRunning = false;
    state.nativeRunning = false;
    setStatus('stopped');
  }
  updateStartStopState();
  refreshDashboardTab();
}

// ─── Node Health panel ────────────────────────────────────────────────────────
// Color map for [data-state] values: ok → green, warn → yellow,
// fail → red, unknown → faint; matches the existing CSS variable palette.
const HEALTH_STATE_COLOR = {
  ok: 'var(--success)',
  warn: 'var(--warning)',
  fail: 'var(--error)',
  unknown: 'var(--text-faint)',
};

function renderHealth(report) {
  if (!report) return;
  state.health = report;
  const paint = (id, dim) => {
    const el = document.getElementById(id);
    if (!el || !dim) return;
    el.textContent = dim.state + (dim.detail ? ' — ' + dim.detail : '');
    el.dataset.state = dim.state;
    el.style.color = HEALTH_STATE_COLOR[dim.state] ?? 'var(--text-faint)';
  };
  paint('health-infra', report.infra);
  paint('health-chain', report.chain);
  paint('health-participation', report.participation);
  refreshStatusPill();
}

// ─── Event listeners ──────────────────────────────────────────────────────────
async function setupListeners() {
  await listen('node-log', (event) => {
    appendLog(event.payload);
  });

  // Single CheckItem per event — merged into state.checks by id.
  await listen('checklist-update', (event) => {
    mergeCheckUpdate(event.payload);
  });

  // Docker pull lifecycle. The backend emits this when the `docker compose
  // pull` process exits — the authoritative "pull is over" signal. Hide the
  // progress panel here rather than relying solely on counting per-image
  // "Pulled" events, which can miss an image's terminal event on some
  // platforms and leave the panel stuck open.
  await listen('pull-complete', (event) => {
    const { gen, success, error } = event.payload || {};
    if (!success) {
      appendLog({ timestamp: '', level: 'ERROR', message: `Pull failed: ${error || 'unknown error'}` });
    }
    // Mark this generation closed so any straggler progress event can't reopen
    // the panel, then hide it if the live session is this pull (or older).
    const closed = Number(gen) || 0;
    state.pullCompletedGen = Math.max(state.pullCompletedGen, closed);
    if (!state.pull || state.pull.gen === undefined || state.pull.gen <= closed) {
      finishPullProgress();
    }
  });

  // Per-image pull bars (docker compose --progress json, aggregated per image).
  await listen('pull-progress', (event) => {
    handlePullProgress(event.payload);
  });

  // Stop lifecycle — surface stop failures in the log drawer; the 10s poll
  // refreshes the status pill.
  await listen('stop-complete', (event) => {
    const { success, error } = event.payload || {};
    if (!success) {
      appendLog({ timestamp: '', level: 'ERROR', message: `Stop failed: ${error || 'unknown error'}` });
    }
  });

  // Postgres rejected the dashboard's password — surface the actionable error
  // and the Reset button on the Dashboard tab.
  await listen('dashboard-db-mismatch', (event) => {
    const { message } = event.payload || {};
    state.dashboardDbError = message || 'Dashboard database password mismatch.';
    appendLog({ timestamp: '', level: 'ERROR', message: state.dashboardDbError });
    refreshDashboardTab();
  });

  // Update notifications
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

  await listen('app-update-available', (event) => {
    const info = event.payload;
    appendLog({ timestamp: '', level: 'INFO', message: `Node Manager v${info.version} available: ${info.url}` });
    showUpdateBadge(info.version, info.url);
  });

  await listen('binary-download-progress', (event) => {
    handleBinaryDownloadProgress(event.payload);
    const { downloaded, total, done } = event.payload;
    const statusEl = document.getElementById('apply-status');
    if (done) {
      if (statusEl) statusEl.textContent = 'Installing native miner\u2026';
    } else if (total) {
      const pct = Math.round((downloaded / total) * 100);
      if (statusEl) statusEl.textContent = `Downloading native miner: ${pct}%`;
    }
  });

  // Health monitor \u2014 event-driven updates from the backend health loop.
  await listen('health-changed', (event) => {
    renderHealth(event.payload);
  });
}

// ─── Version refresh ─────────────────────────────────────────────────────────
async function refreshNodeVersion() {
  try {
    const ver = await invoke('get_app_version');
    const nodeVer = await invoke('get_node_version').catch(() => null);
    const label = nodeVer ? `v${ver} (node ${nodeVer})` : `v${ver}`;
    document.getElementById('app-version').childNodes[0].textContent = `${label} `;
  } catch { /* ignore */ }
}

// ─── Update Badge ─────────────────────────────────────────────────────────────
function showUpdateBadge(version, url) {
  const dot = document.getElementById('update-dot');
  const tooltip = document.getElementById('update-tooltip');
  const tooltipText = document.getElementById('update-tooltip-text');
  const tooltipLink = document.getElementById('update-tooltip-link');
  const versionEl = document.getElementById('app-version');
  if (!dot || !tooltip) return;

  dot.style.display = 'inline-block';
  tooltipText.textContent = `v${version} available`;
  tooltipLink.href = url;
  versionEl.classList.add('has-update');

  versionEl.onclick = (e) => {
    // stopPropagation keeps the toggle click from reaching the document-level
    // hide listener — but it also keeps anchor clicks from reaching the
    // external-link handler below, so the Download link must be handled here.
    e.stopPropagation();
    const anchor = e.target.closest('a[href]');
    if (anchor && anchor.getAttribute('href').startsWith('http')) {
      e.preventDefault();
      openUrl(anchor.getAttribute('href'));
      tooltip.style.display = 'none';
      return;
    }
    tooltip.style.display = tooltip.style.display === 'none' ? 'flex' : 'none';
  };
}

// Hide the update tooltip on any click outside the version badge (badge
// clicks never bubble here — see showUpdateBadge). Registered once at module
// level; showUpdateBadge re-runs on every 30-min update check.
document.addEventListener('click', () => {
  const tooltip = document.getElementById('update-tooltip');
  if (tooltip) tooltip.style.display = 'none';
});

// ─── First-boot prompt ────────────────────────────────────────────────────────
async function checkFirstBoot() {
  const firstBoot = await invoke('is_first_boot');
  if (!firstBoot) return;

  const defaultDir = await invoke('get_default_data_dir');
  const input = document.getElementById('first-boot-dir');
  input.value = defaultDir;

  const modal = document.getElementById('first-boot-modal');
  modal.style.display = '';

  return new Promise((resolve) => {
    document.getElementById('btn-first-boot-continue').addEventListener('click', async () => {
      const dir = input.value.trim() || defaultDir;
      try {
        await invoke('set_data_dir', { path: dir });
        modal.style.display = 'none';
        resolve();
      } catch (e) {
        input.style.borderColor = 'var(--error)';
        input.insertAdjacentHTML('afterend',
          `<div style="color:var(--error);font-size:12px;margin-top:4px;">${e}</div>`);
      }
    });
  });
}

// ─── Open external links in system browser ────────────────────────────────────
document.addEventListener('click', (e) => {
  const anchor = e.target.closest('a[href]');
  if (!anchor) return;
  const href = anchor.getAttribute('href');
  if (href && href.startsWith('http')) {
    e.preventDefault();
    openUrl(href);
  }
});

// ─── Initialize ───────────────────────────────────────────────────────────────
async function init() {
  await checkFirstBoot();

  // Register listeners FIRST so no events are missed
  await setupListeners();

  // Load settings FIRST, before anything that branches on run_mode.
  // pollStatus() reads isDockerMode() synchronously before its first
  // await, so if settings haven't resolved by then it falls back to the
  // 'docker' default — a native-mode user whose node is already running
  // would be probed via get_stack_status, see no running services, and
  // the log-tail reconnect below would be skipped for the entire session.
  try {
    const settings = await invoke('get_settings');
    state.settings = settings;
    populateForm(settings);
    // The Metal toggle (rendered once the hardware survey lands) reflects
    // run_mode; there is no separate run-mode control to seed here.
    if (settings.active_tab && settings.active_tab !== 'status') {
      document
        .querySelector(`[data-tab="${settings.active_tab}"]`)
        ?.click();
    }
  } catch (e) {
    console.error('Failed to load settings:', e);
  }

  // Display app version
  invoke('get_app_version')
    .then((ver) => {
      document.getElementById('app-version').childNodes[0].textContent =
        `v${ver} `;
      invoke('get_node_version')
        .then((nodeVer) => {
          if (nodeVer) {
            document.getElementById('app-version').childNodes[0]
              .textContent = `v${ver} (node ${nodeVer}) `;
          }
        })
        .catch(() => {});
    })
    .catch(() => {});

  // Load storage directory
  invoke('get_data_dir')
    .then((dir) => {
      document.getElementById('data-dir').value = dir;
      state._currentDataDir = dir;
    })
    .catch(() => {});

  // All backend work fires concurrently — no awaits
  invoke('run_hardware_survey')
    .then((survey) => {
      state.hardwareSurvey = survey;
      // Renders the checklist + GPU section (incl. the Metal toggle, whose
      // state reflects run_mode). gpu_backend itself is derived from the
      // survey in collectConfig, so there's nothing to seed here.
      updateRunModeUI();
    })
    .catch(() => {});

  // Seed placeholders from the cache, then kick off a full recheck.
  invoke('get_checklist')
    .then((checks) => {
      for (const c of checks) state.checks.set(c.id, c);
      renderChecklist();
    })
    .catch(console.error)
    .finally(() => {
      invoke('recheck').catch(console.error);
    });

  invoke('check_app_update')
    .then((update) => {
      if (update) showUpdateBadge(update.version, update.url);
    })
    .catch(() => {});

  // Poll status — also fire-and-forget, handles log stream reconnect
  pollStatus().then(() => {
    const running = state.containerRunning || state.nativeRunning;
    if (running) {
      collapseConfig();
      if (isDockerMode()) {
        invoke('start_log_stream').catch(console.error);
      } else {
        invoke('start_native_log_tail').catch(console.error);
      }
    }
  });

  state.pollInterval = setInterval(pollStatus, 10_000);

  // Fallback health poll — the health-changed event is the primary path;
  // this catches any missed events (e.g. listener registered after first emit).
  setInterval(async () => {
    try { renderHealth(await invoke('get_health')); } catch (_) { /* transient — backend may not have sampled yet */ }
  }, 15_000);
}

init().catch(console.error);
