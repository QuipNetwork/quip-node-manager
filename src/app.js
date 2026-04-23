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
  checksPassed: false,
  detectedGpus: [], // { index, name }
  logLines: [],
  MAX_LOG_LINES: 500,
  pollInterval: null,
  // Map<id, CheckItem> — single source of truth for the checklist UI.
  // Merged from `checklist-update` events; rendered by renderChecklist().
  checks: new Map(),
  hardwareSurvey: null,
};

// ─── Stack Configuration UI ─────────────────────────────────────────────────

// Show the TLS subsettings block only when both dashboard and TLS are on.
// TLS without the dashboard is meaningless (Caddy only fronts the dashboard
// in this stack) so we disable the TLS checkbox when dashboard is off.
function updateStackUiVisibility() {
  const dashEl = document.getElementById('dashboard-enabled');
  const tlsEl = document.getElementById('tls-enabled');
  const subs = document.getElementById('tls-subsettings');
  if (!dashEl || !tlsEl || !subs) return;

  const dash = dashEl.checked;
  tlsEl.disabled = !dash;
  if (!dash) tlsEl.checked = false;
  subs.style.display = dash && tlsEl.checked ? '' : 'none';
}

document.addEventListener('change', (e) => {
  if (!e.target) return;
  if (e.target.id === 'dashboard-enabled' || e.target.id === 'tls-enabled') {
    updateStackUiVisibility();
  }
  if (e.target.id === 'dashboard-enabled' || e.target.id === 'tls-enabled') {
    // These settings change the compose profile; refresh the checklist so
    // the user sees profile-specific items (rest-port-native, port-tls, …)
    // without waiting for the next recheck cycle.
    invoke('recheck').catch(() => {});
  }
});

// ─── Dashboard tab iframe wiring ────────────────────────────────────────────

function dashboardUrl(settings) {
  if (!settings?.dashboard_enabled) return null;
  const hostname = settings.dashboard_hostname || 'localhost:20080';
  // ACME via Caddy only when TLS is on AND the hostname is a real DNS name
  // (localhost can't get a public cert). In every other case plain HTTP on
  // whatever port was configured.
  if (settings.tls_enabled && !hostname.startsWith('localhost')) {
    return `https://${hostname.replace(/:.*/, '')}`;
  }
  return `http://${hostname}`;
}

function refreshDashboardTab() {
  const frame = document.getElementById('dashboard-frame');
  const empty = document.getElementById('dashboard-empty');
  const msg = document.getElementById('dashboard-empty-msg');
  if (!frame || !empty) return; // tab markup not present on first load

  const url = dashboardUrl(state.settings);
  const dashRunning = state.stack?.services?.some(
    (s) => (s.service === 'dashboard' || s.service === 'dashboard-direct') && s.running,
  );

  // Toggle via style.display rather than the `hidden` attribute — the
  // placeholder has `display: flex` in CSS (for its centered layout) which
  // would otherwise override `hidden` and keep both elements visible.
  const show = (el, display) => { el.style.display = display; };

  if (!url) {
    if (msg) msg.textContent = 'Dashboard disabled — enable it in the Status tab.';
    show(empty, 'flex');
    show(frame, 'none');
    if (frame.src !== 'about:blank') frame.src = 'about:blank';
  } else if (!dashRunning) {
    if (msg) msg.textContent = 'Starting dashboard…';
    show(empty, 'flex');
    show(frame, 'none');
    if (frame.src !== 'about:blank') frame.src = 'about:blank';
  } else {
    // Only reload the iframe when the URL actually changes — prevents
    // flicker on every poll tick.
    if (frame.src !== url) frame.src = url;
    show(empty, 'none');
    show(frame, 'block');
  }
}

// Render order; the backend decides visibility, so this array can safely
// include ids that don't appear in state.checks for the current settings
// combo (they're skipped). Mirrors ALL_CHECK_IDS in checklist.rs.
const CHECK_ORDER = [
  'docker', 'docker-compose', 'stack-assets', 'wsl',
  'stack-images', 'binary', 'version', 'secret',
  'ip', 'hostname', 'port', 'port-dashboard', 'port-tls',
  'rest-port-native', 'firewall', 'dwave-key',
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
  PullImage:       'Pull Image',
  DownloadBinary:  'Download & Install',
  GenerateSecret:  'Generate Secret',
  Delegate:        'Update',
};

// ─── Tab switching ──────────────────────────────────────────────────────────
document.querySelectorAll('.tab-btn').forEach((btn) => {
  btn.addEventListener('click', () => {
    const tab = btn.dataset.tab;
    document
      .querySelectorAll('.tab-btn')
      .forEach((b) => b.classList.remove('active'));
    document
      .querySelectorAll('.tab-content')
      .forEach((c) => c.classList.remove('active'));
    btn.classList.add('active');
    document.getElementById(`tab-${tab}`).classList.add('active');
    if (state.settings) {
      state.settings.active_tab = tab;
      invoke('update_settings', { settings: state.settings }).catch(
        console.error
      );
    }
    if (tab === 'dashboard') refreshDashboardTab();
  });
});

// ─── Configuration section toggle ────────────────────────────────────────────
document.getElementById('btn-config-toggle').addEventListener('click', () => {
  const btn = document.getElementById('btn-config-toggle');
  const section = document.getElementById('config-section');
  const expanded = btn.getAttribute('aria-expanded') === 'true';
  btn.setAttribute('aria-expanded', String(!expanded));
  section.style.display = expanded ? 'none' : '';
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
    await invoke('recheck', { ids: ['port', 'firewall'] }).catch(console.error);
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

// ─── Run mode select ─────────────────────────────────────────────────────────
document.getElementById('run-mode-select').addEventListener('change', async () => {
  if (!state.settings) return;
  state.settings.run_mode = document.getElementById('run-mode-select').value;
  updateRunModeUI();
  await invoke('update_settings', { settings: state.settings }).catch(console.error);
  // Mode change invalidates the whole cache — backend reseeds and reruns.
  state.checks.clear();
  await invoke('recheck').catch(console.error);
});

function updateRunModeUI() {
  const survey = state.hardwareSurvey;
  const isMac = survey?.os === 'macos';

  // Run Mode toggle is only available on macOS
  const runModeGroup = document.getElementById('run-mode-group');
  if (runModeGroup) {
    runModeGroup.style.display = isMac ? '' : 'none';
  }

  // Force Docker on non-macOS
  if (!isMac && state.settings) {
    state.settings.run_mode = 'docker';
  }

  const mode = state.settings?.run_mode || 'docker';
  const isDocker = mode === 'docker';

  // Checklist items are filtered by mode inside renderChecklist(); re-render
  // here because mode and the hardware survey (WSL visibility) can land
  // in either order during init.
  renderChecklist();

  // Warnings (only relevant on macOS where the toggle exists)
  const warning = document.getElementById('run-mode-warning');
  if (isDocker && isMac) {
    warning.textContent = '\u26A0 Mac Metal GPUs are not accessible in Docker.';
    warning.style.display = '';
  } else {
    warning.style.display = 'none';
  }

  renderGpuDevices();
}

function renderGpuDevices() {
  const list = document.getElementById('gpu-device-list');
  const noDevices = document.getElementById('gpu-no-devices');
  const globalSettings = document.getElementById('gpu-global-settings');
  const survey = state.hardwareSurvey;
  const devices = survey?.gpu_devices || [];

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
    const backendLabel = survey.gpu_backend === 'metal' ? 'Metal' : 'CUDA';

    const row = document.createElement('div');
    row.style.cssText = 'display:flex;align-items:center;gap:10px;padding:6px 0;';

    const label = document.createElement('label');
    label.className = 'gpu-toggle-switch';
    const checkbox = document.createElement('input');
    checkbox.type = 'checkbox';
    checkbox.className = 'gpu-device-toggle';
    checkbox.dataset.index = String(dev.index);
    checkbox.checked = enabled;
    const slider = document.createElement('span');
    slider.className = 'gpu-toggle-slider';
    label.appendChild(checkbox);
    label.appendChild(slider);

    const text = document.createElement('span');
    text.style.fontSize = '13px';
    text.textContent = `GPU ${dev.index}: ${dev.name} (${backendLabel})${mem}`;

    row.appendChild(label);
    row.appendChild(text);
    list.appendChild(row);
  });
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

// ─── QPU section toggle ───────────────────────────────────────────────────────
document.getElementById('btn-qpu-toggle').addEventListener('click', () => {
  const section = document.getElementById('qpu-section');
  const btn = document.getElementById('btn-qpu-toggle');
  const isVisible = section.style.display !== 'none';
  section.style.display = isVisible ? 'none' : 'block';
  btn.textContent = isVisible
    ? 'Have D-Wave / QPU Access? Click here'
    : 'Hide QPU Configuration';
});

// ─── GPU utilization slider ──────────────────────────────────────────────────
document.getElementById('gpu-utilization').addEventListener('input', () => {
  const val = document.getElementById('gpu-utilization').value;
  document.getElementById('gpu-util-display').textContent = `${val}%`;
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

  const fanoutRaw = document.getElementById('fanout')?.value?.trim();
  const fanout = fanoutRaw ? (parseInt(fanoutRaw) || null) : null;

  const base = state.settings?.node_config ?? {};

  return {
    port: parseInt(document.getElementById('port').value) || 20049,
    listen: document.getElementById('listen')?.value?.trim() || '::',
    public_host: document.getElementById('public-host-enable')?.checked
      ? document.getElementById('public-host')?.value?.trim() ?? ''
      : '',
    public_port: document.getElementById('public-host-enable')?.checked
      ? (parseInt(document.getElementById('public-port')?.value) || null)
      : null,
    node_name: document.getElementById('node-name')?.value?.trim() ?? '',
    peers: document
      .getElementById('peers')
      .value.split('\n')
      .map((s) => s.trim())
      .filter((s) => s.length > 0),
    auto_mine: document.getElementById('auto-mine')?.checked ?? false,
    secret: state.settings?.node_config?.secret ?? '',
    genesis_config: base.genesis_config ?? 'genesis_block.json',
    tofu: base.tofu ?? true,
    trust_db: base.trust_db ?? '~/.quip/trust.db',
    tls_cert_file: document.getElementById('tls-cert-file')?.value?.trim() ?? '',
    tls_key_file: document.getElementById('tls-key-file')?.value?.trim() ?? '',
    verify_tls: document.getElementById('verify-tls')?.checked ?? false,
    rest_host: document.getElementById('rest-host')?.value?.trim() ?? '127.0.0.1',
    rest_port: parseInt(document.getElementById('rest-port')?.value) ?? -1,
    rest_insecure_port: parseInt(document.getElementById('rest-insecure-port')?.value) ?? -1,
    telemetry_enabled: document.getElementById('telemetry-enabled')?.checked ?? true,
    telemetry_dir: document.getElementById('telemetry-dir')?.value?.trim() ?? 'telemetry',
    log_level: document.getElementById('log-level')?.value || 'info',
    node_log: document.getElementById('node-log')?.value?.trim() ?? '',
    http_log: document.getElementById('http-log')?.value?.trim() ?? '',
    num_cpus: parseInt(document.getElementById('num-cpus').value) || 1,
    gpu_backend: gpuBackend,
    gpu_device_configs: gpuDeviceConfigs,
    dwave_config: dwaveConfig,
    timeout: parseInt(document.getElementById('timeout')?.value) || 3,
    heartbeat_interval:
      parseInt(document.getElementById('heartbeat-interval')?.value) || 15,
    heartbeat_timeout:
      parseInt(document.getElementById('heartbeat-timeout')?.value) || 300,
    fanout,
  };
}

// ─── Apply form → settings ────────────────────────────────────────────────────
function applyFormToSettings() {
  if (!state.settings) return;
  state.settings.node_config = collectConfig();
  state.settings.auto_update_enabled =
    document.getElementById('auto-update-enabled')?.checked ?? false;

  // Image is auto-derived from the GPU config: CUDA when any NVIDIA GPU is
  // enabled, CPU otherwise. QPU mining is a config.toml [dwave] concern, not
  // a separate image — so there's no QPU option to pick here.
  const hasEnabledCuda = (state.settings.node_config.gpu_device_configs || [])
    .some((d) => d.enabled) && state.hardwareSurvey?.gpu_backend === 'cuda';
  state.settings.image_tag = hasEnabledCuda ? 'cuda' : 'cpu';

  state.settings.dashboard_enabled =
    document.getElementById('dashboard-enabled')?.checked ?? true;
  state.settings.tls_enabled =
    document.getElementById('tls-enabled')?.checked ?? false;
  state.settings.dashboard_hostname =
    document.getElementById('dashboard-hostname')?.value?.trim() ||
    'localhost:20080';
  state.settings.cert_email =
    document.getElementById('cert-email')?.value?.trim() || '';
  state.settings.zerossl_api_key =
    document.getElementById('zerossl-api-key')?.value ?? '';
}

// ─── Populate form from settings ─────────────────────────────────────────────
function populateForm(settings) {
  const c = settings.node_config;

  // Node Configuration
  document.getElementById('port').value = c.port ?? 20049;
  document.getElementById('listen').value = c.listen || '::';
  document.getElementById('secret-display').value = c.secret ?? '';
  document.getElementById('auto-mine').checked = c.auto_mine ?? false;

  // Custom settings
  document.getElementById('node-name').value = c.node_name ?? '';
  const publicHost = c.public_host ?? '';
  const publicPort = c.public_port ?? null;
  if (publicHost || publicPort) {
    document.getElementById('public-host-enable').checked = true;
    document.getElementById('public-host').disabled = false;
    document.getElementById('public-port').disabled = false;
    document.getElementById('public-host').value = publicHost;
    document.getElementById('public-port').value = publicPort ?? '';
  }
  document.getElementById('peers').value = (c.peers || []).join('\n');
  document.getElementById('timeout').value = c.timeout ?? 3;
  document.getElementById('heartbeat-interval').value =
    c.heartbeat_interval ?? 15;
  document.getElementById('heartbeat-timeout').value =
    c.heartbeat_timeout ?? 300;
  if (c.fanout != null) {
    document.getElementById('fanout').value = c.fanout;
  }
  document.getElementById('log-level').value = c.log_level ?? 'info';
  document.getElementById('verify-tls').checked = c.verify_tls ?? false;

  // New fields
  document.getElementById('telemetry-enabled').checked = c.telemetry_enabled ?? true;
  document.getElementById('telemetry-dir').value = c.telemetry_dir ?? 'telemetry';
  document.getElementById('tls-cert-file').value = c.tls_cert_file ?? '';
  document.getElementById('tls-key-file').value = c.tls_key_file ?? '';
  document.getElementById('rest-host').value = c.rest_host ?? '127.0.0.1';
  document.getElementById('rest-port').value = c.rest_port ?? -1;
  document.getElementById('rest-insecure-port').value = c.rest_insecure_port ?? -1;
  document.getElementById('node-log').value = c.node_log ?? '';
  document.getElementById('http-log').value = c.http_log ?? '';

  // Stack Configuration (image_tag is auto-derived, no UI control)
  document.getElementById('dashboard-enabled').checked =
    settings.dashboard_enabled ?? true;
  document.getElementById('tls-enabled').checked =
    settings.tls_enabled ?? false;
  document.getElementById('dashboard-hostname').value =
    settings.dashboard_hostname ?? 'localhost:20080';
  document.getElementById('cert-email').value = settings.cert_email ?? '';
  document.getElementById('zerossl-api-key').value =
    settings.zerossl_api_key ?? '';
  updateStackUiVisibility();

  // Auto-expand custom settings if any non-default values are set
  const hasCustom =
    publicHost || publicPort ||
    (c.peers || []).length > 0 ||
    c.timeout !== 3 ||
    c.heartbeat_interval !== 15 ||
    c.heartbeat_timeout !== 300 ||
    c.fanout != null ||
    c.log_level !== 'info' ||
    (c.verify_tls ?? false) ||
    (c.tls_cert_file ?? '') ||
    (c.rest_port ?? -1) > 0;
  if (hasCustom) {
    document.getElementById('btn-custom-toggle').setAttribute('aria-expanded', 'true');
    document.getElementById('custom-settings-section').style.display = '';
  }

  // CPU Miner
  document.getElementById('num-cpus').value = c.num_cpus ?? 1;

  // GPU Miner — utilization/yielding from first enabled device or defaults
  const gpuCfg = (c.gpu_device_configs || []).find((d) => d.enabled) || (c.gpu_device_configs || [])[0];
  const savedUtil = gpuCfg?.utilization ?? 80;
  document.getElementById('gpu-utilization').value = savedUtil;
  document.getElementById('gpu-util-display').textContent = `${savedUtil}%`;
  document.getElementById('gpu-yielding').checked = gpuCfg?.yielding ?? false;

  // QPU / D-Wave
  const dw = c.dwave_config;
  if (dw) {
    document.getElementById('qpu-api-key').value = dw.token ?? '';
    document.getElementById('qpu-daily-budget').value = dw.daily_budget ?? '';
    if (dw.token) {
      document.getElementById('qpu-section').style.display = 'block';
      document.getElementById('btn-qpu-toggle').textContent =
        'Hide QPU Configuration';
    }
  }
  // GPU device list rendered after list_gpu_devices call in init
}

// ─── Start/Stop/Apply enable state ───────────────────────────────────────────
function updateStartStopState() {
  const running = state.containerRunning || state.nativeRunning;
  document.getElementById('btn-start').disabled =
    !state.checksPassed || running;
  document.getElementById('btn-stop').disabled = !running;
  document.getElementById('btn-apply').disabled = !state.checksPassed;
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
    sub.textContent = 'Running but some checks failing';
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
  const dashboard = s?.dashboard_enabled ?? true;
  const tls = s?.tls_enabled ?? false;
  const hasDwave = !!s?.node_config?.dwave_config;
  // Compose runs whenever we're in Docker mode, OR Native mode with the
  // dashboard on (dashboard+postgres[+caddy] run via compose even when
  // the node runs as a host binary).
  const composeWillRun = isDocker || dashboard;

  switch (id) {
    case 'docker':
    case 'docker-compose':
    case 'stack-assets':
    case 'stack-images':
      return composeWillRun;
    case 'wsl':
      return isDocker && state.hardwareSurvey?.os === 'windows';
    case 'binary':
      return !isDocker;
    case 'port-dashboard':
      return composeWillRun && dashboard && !tls;
    case 'port-tls':
      return composeWillRun && dashboard && tls;
    case 'rest-port-native':
      return !isDocker && dashboard;
    case 'dwave-key':
      return hasDwave;
    // version / secret / ip / hostname / port / firewall — always shown.
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
  recheckBtn.textContent = item.state === 'running' ? 'Checking…' : 'Recheck';
  recheckBtn.disabled = item.state === 'running';
  actions.appendChild(recheckBtn);

  if (item.fixable && (item.state === 'fail' || item.state === 'warn')) {
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
  switch (id) {
    case 'docker':            return 'Docker installed & running';
    case 'docker-compose':    return 'Docker Compose v2 available';
    case 'stack-assets':      return 'Stack files staged (compose.yml + Caddyfile)';
    case 'wsl':               return 'WSL installed with distro';
    case 'stack-images':      return 'Stack images available';
    case 'binary':            return 'Node binary available';
    case 'version':           return 'Node version up to date';
    case 'secret':            return 'Node secret configured';
    case 'ip':                return 'Public IP reachable';
    case 'hostname':          return 'Hostname accessible to internet';
    case 'port':              return `Port ${port} — press Recheck to test`;
    case 'port-dashboard':    return 'Dashboard port 20080 available';
    case 'port-tls':          return 'TLS ports 80 + 443 available';
    case 'rest-port-native':  return 'Native REST port available';
    case 'firewall':          return 'Local firewall allows port (UDP+TCP)';
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

  // The version check resolves the "v<app> (node <node>)" label in the
  // header. When version transitions to a terminal state the node version
  // may have changed (pull/download fixes), so refresh the display.
  if (item.id === 'version' && item.state !== 'idle' && item.state !== 'running') {
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

    case 'PullImage': {
      // Pulls every image in the current profile (node + dashboard +
      // postgres + caddy as applicable). The old tag-specific call is
      // obsolete — image selection happens inside compose now.
      try {
        await invoke('pull_compose_images');
      } catch (e) {
        console.error('Pull failed:', e);
      }
      return;
    }

    case 'DownloadBinary':
      try {
        await invoke('download_native_binary');
      } catch (e) {
        appendLog({ timestamp: '', level: 'ERROR', message: `Download failed: ${e}` });
      }
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

    case 'Delegate':
      return runFix(fix.arg);
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
    invoke('recheck', { ids: [id] }).catch(console.error);
  } else if (btn.dataset.action === 'fix') {
    runFix(id);
  }
});

// ─── Global Recheck All ──────────────────────────────────────────────────────
document.getElementById('btn-recheck-all').addEventListener('click', () => {
  invoke('recheck').catch(console.error);
});

// ─── Log panel ────────────────────────────────────────────────────────────────
function escHtml(str) {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
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
  line.textContent = `${ts}${entry.message}`;
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

async function startNode() {
  if (isDockerMode()) {
    await invoke('start_stack');
    await invoke('start_log_stream');
  } else {
    // Native mode: run the binary on the host + (optionally) the
    // compose stack's non-node services (dashboard+postgres+caddy) so
    // the user still gets the dashboard UI.
    await invoke('start_native_node');
    if (state.settings?.dashboard_enabled) {
      try { await invoke('start_stack'); } catch (e) { console.error('stack start:', e); }
    }
  }
  collapseConfig();
}

async function stopNode() {
  if (isDockerMode()) {
    await invoke('stop_log_stream');
    await invoke('stop_stack');
  } else {
    await invoke('stop_native_node');
    if (state.settings?.dashboard_enabled) {
      try { await invoke('stop_stack'); } catch (e) { console.error('stack stop:', e); }
    }
  }
}

// ─── Start / Stop ─────────────────────────────────────────────────────────────
document.getElementById('btn-start').addEventListener('click', async () => {
  applyFormToSettings();
  const applyStatus = document.getElementById('apply-status');
  applyStatus.textContent = 'Starting\u2026';
  try {
    await invoke('update_settings', { settings: state.settings });
    await startNode();
    applyStatus.textContent = 'Node started.';
    await pollStatus();
  } catch (e) {
    applyStatus.textContent = `Error: ${e}`;
  }
});

document.getElementById('btn-stop').addEventListener('click', async () => {
  try {
    await stopNode();
    state.containerRunning = false;
    state.nativeRunning = false;
    setStatus('stopped');
    updateStartStopState();
    expandConfig();
  } catch (e) {
    console.error(e);
  }
});

// ─── Apply & Restart ──────────────────────────────────────────────────────────
document.getElementById('btn-apply').addEventListener('click', async () => {
  applyFormToSettings();
  const applyStatus = document.getElementById('apply-status');
  applyStatus.textContent = 'Applying\u2026';
  try {
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
      (x) => ['cpu', 'cuda', 'qpu'].includes(x.service) && x.running,
    );
    return !!nodeRunning;
  }
  return s.services?.some((x) => x.running);
}

async function pollStatus() {
  try {
    // Stack status is valid in both Docker and Native modes (Native runs a
    // subset — dashboard+postgres[+caddy]).
    try {
      state.stack = await invoke('get_stack_status');
    } catch {
      state.stack = null;
    }

    if (isDockerMode()) {
      state.containerRunning = stackRunningInMode();
      state.nativeRunning = false;
      setStatus(state.containerRunning ? 'running' : 'stopped');
    } else {
      const status = await invoke('get_native_node_status');
      state.nativeRunning = status.running;
      state.containerRunning = false;
      setStatus(status.running ? 'running' : 'stopped');
    }
  } catch {
    state.containerRunning = false;
    state.nativeRunning = false;
    setStatus('stopped');
  }
  updateStartStopState();
  refreshDashboardTab();
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

  await listen('node-status', (event) => {
    const { state: s } = event.payload;
    const stateStr = s?.toLowerCase() || 'stopped';
    state.containerRunning = stateStr === 'running';
    setStatus(stateStr);
    updateStartStopState();
  });

  // Docker pull lifecycle — pull-complete always triggers a backend-side
  // recheck of image+version, so the UI auto-updates without us doing
  // anything here beyond logging terminal outcomes.
  await listen('pull-complete', (event) => {
    const { success, error } = event.payload || {};
    if (!success) {
      appendLog({ timestamp: '', level: 'ERROR', message: `Pull failed: ${error || 'unknown error'}` });
    }
  });

  // Stop lifecycle — update the status pill immediately; backend also
  // emits container-status so we don't have to wait for the next poll.
  await listen('stop-complete', (event) => {
    const { success, error } = event.payload || {};
    if (!success) {
      appendLog({ timestamp: '', level: 'ERROR', message: `Stop failed: ${error || 'unknown error'}` });
    }
  });

  await listen('stack-status', (event) => {
    // Backend may emit lifecycle events in future; for now we just re-poll
    // to refresh state.stack and the dashboard iframe visibility.
    void event;
    pollStatus();
  });

  // Update notifications
  await listen('image-update-available', () => {
    appendLog({ timestamp: '', level: 'INFO', message: 'New Docker image available. Restart to update.' });
    refreshNodeVersion();
  });

  await listen('binary-update-available', (event) => {
    const info = event.payload;
    appendLog({ timestamp: '', level: 'INFO', message: `New binary v${info.version} available. Download to update.` });
    refreshNodeVersion();
  });

  await listen('app-update-available', (event) => {
    const info = event.payload;
    appendLog({ timestamp: '', level: 'INFO', message: `Node Manager v${info.version} available: ${info.url}` });
    showUpdateBadge(info.version, info.url);
  });

  await listen('binary-download-progress', (event) => {
    const { downloaded, total, done } = event.payload;
    const statusEl = document.getElementById('apply-status');
    if (done) {
      if (statusEl) statusEl.textContent = 'Installing binary\u2026';
    } else if (total) {
      const pct = Math.round((downloaded / total) * 100);
      if (statusEl) statusEl.textContent = `Downloading binary: ${pct}%`;
    }
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
    e.stopPropagation();
    tooltip.style.display = tooltip.style.display === 'none' ? 'flex' : 'none';
  };
  document.addEventListener('click', () => {
    tooltip.style.display = 'none';
  }, { once: false });
}

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
    document.getElementById('run-mode-select').value =
      settings.run_mode || 'docker';
    document.getElementById('auto-update-enabled').checked =
      settings.auto_update_enabled ?? false;
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
      updateRunModeUI();
      const noSavedGpu =
        !(state.settings?.node_config?.gpu_device_configs || []).some(
          (d) => d.enabled
        ) && state.settings?.node_config?.gpu_backend !== 'mps';
      if (noSavedGpu && survey.gpu_backend !== 'none') {
        document.getElementById('gpu-backend').value = survey.gpu_backend;
      }
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
}

init().catch(console.error);
