// SPDX-License-Identifier: AGPL-3.0-or-later

// Tauri IPC bridge
const invoke =
  window.__TAURI__?.core?.invoke ??
  (() => Promise.reject('Tauri not available'));
const listen =
  window.__TAURI__?.event?.listen ?? (() => Promise.resolve(() => {}));
const openUrl =
  window.__TAURI__?.opener?.openUrl ??
  ((url) => { window.open(url, '_blank'); });

// App state
const state = {
  settings: null,
  containerRunning: false,
  nativeRunning: false,
  checksPassed: false,
  detectedGpus: [], // { index, name }
  logLines: [],
  MAX_LOG_LINES: 500,
  pollInterval: null,
  portCheckResult: null, // null=unchecked, true=forwarded, false=not reached
  lastChecks: [],        // last checklist payload from backend
  hardwareSurvey: null,
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

// ─── Port change → re-run checklist ──────────────────────────────────────────
document.getElementById('port').addEventListener('change', async () => {
  const port = parseInt(document.getElementById('port').value) || 20049;
  if (state.settings) {
    state.settings.node_config.port = port;
    await invoke('update_settings', { settings: state.settings }).catch(console.error);
    await invoke('run_checklist').catch(console.error);
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
  await invoke('run_checklist').catch(console.error);
});

function updateRunModeUI() {
  const mode = state.settings?.run_mode || 'docker';
  const isDocker = mode === 'docker';

  // Toggle checklist item visibility
  document.querySelectorAll('.docker-only').forEach((el) => {
    el.style.display = isDocker ? '' : 'none';
  });
  document.querySelectorAll('.native-only').forEach((el) => {
    el.style.display = isDocker ? 'none' : '';
  });

  // Warnings
  const warning = document.getElementById('run-mode-warning');
  const survey = state.hardwareSurvey;
  if (!isDocker && survey?.os !== 'macos') {
    warning.textContent = '\u26A0 Docker provides better isolation and security.';
    warning.style.display = '';
  } else if (isDocker && survey?.os === 'macos') {
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
    await invoke('run_checklist').catch(console.error);
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
  // cuda image only when NVIDIA GPUs enabled in Docker mode
  const hasEnabledCuda = (state.settings.node_config.gpu_device_configs || [])
    .some((d) => d.enabled) && state.hardwareSurvey?.gpu_backend === 'cuda';
  state.settings.image_tag = hasEnabledCuda ? 'cuda' : 'cpu';
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

// ─── Checklist update ─────────────────────────────────────────────────────────
function updateChecklist(checks) {
  // Checklist streams in progressively; complete once the last item arrives.
  const complete = checks.some((c) => c.id === 'firewall');
  state.lastChecks = checks;
  const portPassed = (c) =>
    state.portCheckResult !== null ? state.portCheckResult : c.passed;
  const checkPassed = (c) =>
    c.id === 'port' ? portPassed(c) : c.passed;
  const allPassed =
    complete &&
    checks.filter((c) => c.required !== false).every(checkPassed);
  const requiredFailing = checks.filter(
    (c) => c.required !== false && !checkPassed(c)
  ).length;
  const warnings = checks.filter(
    (c) => c.required === false && !checkPassed(c) && !c.label?.includes('\u2026')
  ).length;

  state.checksPassed = allPassed;

  const summary = document.getElementById('checklist-summary');
  const checklistEl = document.getElementById('checklist');
  const toggleBtn = document.getElementById('checklist-toggle');

  if (summary) {
    if (!complete) {
      summary.textContent = 'Checking\u2026';
      summary.style.color = 'var(--text-faint)';
    } else if (allPassed && warnings === 0) {
      summary.textContent = '\u2713 All requirements met';
      summary.style.color = 'var(--success)';
      toggleBtn.setAttribute('aria-expanded', 'false');
      checklistEl.style.display = 'none';
    } else if (allPassed && warnings > 0) {
      const s = warnings > 1 ? 's' : '';
      summary.textContent = `\u2713 Ready (${warnings} warning${s})`;
      summary.style.color = 'var(--warning)';
      toggleBtn.setAttribute('aria-expanded', 'false');
      checklistEl.style.display = 'none';
    } else {
      summary.textContent = `\u2717 ${requiredFailing} not met`;
      summary.style.color = 'var(--error)';
      toggleBtn.setAttribute('aria-expanded', 'true');
      checklistEl.style.display = '';
    }
  }

  checks.forEach((check) => {
    const item = document.querySelector(
      `.checklist-item[data-id="${check.id}"]`
    );
    if (!item) return;
    const icon = item.querySelector('.check-icon');
    const label = item.querySelector('.check-label');

    const passed = checkPassed(check);
    const isPending = !passed && check.label?.includes('\u2026');
    const isWarning = !passed && !isPending && check.required === false;
    icon.textContent = passed
      ? '\u2713'
      : isPending ? '\u25CB' : (isWarning ? '\u26A0' : '\u2717');
    icon.style.color = passed
      ? 'var(--success)'
      : isPending ? 'var(--text-faint)' : (isWarning ? 'var(--warning)' : 'var(--error)');
    if (label && check.label) label.textContent = check.label;

    const actionBtn = item.querySelector('.check-action');
    if (actionBtn && actionBtn.tagName === 'BUTTON') {
      actionBtn.style.display = (passed || isPending) ? 'none' : 'inline-flex';
    }
  });

  updateStartStopState();
}

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

// ─── Checklist action buttons ──────────────────────────────────────────────────
document
  .querySelector('[data-id="docker"] .check-action')
  ?.addEventListener('click', () => {
    openUrl('https://docs.docker.com/get-docker/');
  });

document
  .querySelector('[data-id="image"] .check-action')
  ?.addEventListener('click', async () => {
    const btn = document.querySelector('[data-id="image"] .check-action');
    const tag = state.settings?.image_tag || 'cpu';
    btn.disabled = true;
    btn.textContent = 'Pulling\u2026';
    try {
      await invoke('pull_node_image', { imageTag: tag });
      btn.style.display = 'none';
      await invoke('run_checklist');
    } catch (e) {
      btn.disabled = false;
      btn.textContent = 'Retry Pull';
      appendLog({ timestamp: '', level: 'ERROR', message: `Pull failed: ${e}` });
    }
  });

document
  .querySelector('[data-id="secret"] .check-action')
  ?.addEventListener('click', async () => {
    const btn = document.querySelector('[data-id="secret"] .check-action');
    btn.disabled = true;
    btn.textContent = 'Generating\u2026';
    try {
      const secret = await invoke('generate_node_secret');
      document.getElementById('secret-display').value = secret;
      if (state.settings) {
        state.settings.node_config.secret = secret;
        await invoke('update_settings', { settings: state.settings });
      }
      btn.style.display = 'none';
      await invoke('run_checklist');
    } catch (e) {
      btn.disabled = false;
      btn.textContent = 'Generate Secret';
      console.error(e);
    }
  });

// ─── Binary download action ──────────────────────────────────────────────────
document
  .querySelector('[data-id="binary"] .check-action')
  ?.addEventListener('click', async () => {
    const btn = document.querySelector('[data-id="binary"] .check-action');
    const label = document.querySelector('[data-id="binary"] .check-label');
    btn.disabled = true;
    btn.textContent = 'Downloading\u2026';
    label.textContent = 'Downloading node binary\u2026';
    // Switch label to "Installing" once download completes (before invoke returns)
    const progressCleanup = await listen('binary-download-progress', (event) => {
      if (event.payload.done) {
        label.textContent = 'Installing node binary\u2026';
        btn.textContent = 'Installing\u2026';
      }
    });
    try {
      const version = await invoke('download_native_binary');
      label.textContent = `Node binary v${version} installed`;
      btn.style.display = 'none';
      progressCleanup();
      await invoke('run_checklist');
    } catch (e) {
      label.textContent = `Download failed: ${e}`;
      btn.disabled = false;
      btn.textContent = 'Retry Download & Install';
      progressCleanup();
    }
  });

// ─── Version update action (delegates to image pull or binary download) ──────
document
  .querySelector('[data-id="version"] .check-action')
  ?.addEventListener('click', () => {
    const target = isDockerMode()
      ? document.querySelector('[data-id="image"] .check-action')
      : document.querySelector('[data-id="binary"] .check-action');
    if (target) target.click();
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
    await invoke('start_node_container');
    await invoke('start_log_stream');
  } else {
    await invoke('start_native_node');
  }
  collapseConfig();
}

async function stopNode() {
  if (isDockerMode()) {
    await invoke('stop_log_stream');
    await invoke('stop_node_container');
  } else {
    await invoke('stop_native_node');
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
async function pollStatus() {
  try {
    if (isDockerMode()) {
      const status = await invoke('get_container_status');
      state.containerRunning = status.running;
      state.nativeRunning = false;
      setStatus(status.running ? 'running' : 'stopped');
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
}

// ─── Port recheck ─────────────────────────────────────────────────────────────
document.getElementById('btn-port-recheck').addEventListener('click', async () => {
  const btn = document.getElementById('btn-port-recheck');
  const item = document.querySelector('.checklist-item[data-id="port"]');
  const icon = item.querySelector('.check-icon');
  const label = item.querySelector('.check-label');
  const port = state.settings?.node_config?.port ?? 20049;

  btn.disabled = true;
  btn.textContent = 'Checking\u2026';
  icon.textContent = '\u25cb';
  icon.style.color = '';
  label.textContent = `Port ${port} \u2014 checking via public IP\u2026`;

  try {
    const ok = await invoke('recheck_port_forwarding', { port });
    state.portCheckResult = ok;
    icon.textContent = ok ? '\u2713' : '\u2717';
    icon.style.color = ok ? 'var(--success)' : 'var(--error)';
    label.textContent = ok
      ? `Port ${port} forwarded (ensure both UDP+TCP on router)`
      : `Port ${port} \u2014 not reachable \u2014 forward UDP+TCP on router`;
    // Re-evaluate summary and start-button state using the stored checks.
    updateChecklist(state.lastChecks);
  } catch (e) {
    label.textContent = `Port check error: ${e}`;
  } finally {
    btn.disabled = false;
    btn.textContent = 'Recheck';
  }
});

// ─── Event listeners ──────────────────────────────────────────────────────────
async function setupListeners() {
  await listen('node-log', (event) => {
    appendLog(event.payload);
  });

  await listen('checklist-update', (event) => {
    updateChecklist(event.payload);
  });

  await listen('node-status', (event) => {
    const { state: s } = event.payload;
    const stateStr = s?.toLowerCase() || 'stopped';
    state.containerRunning = stateStr === 'running';
    setStatus(stateStr);
    updateStartStopState();
  });

  // Background version check result — patches the placeholder in the checklist
  await listen('version-check-update', (event) => {
    const result = event.payload;
    const checks = state.lastChecks;
    if (!checks) return;
    const idx = checks.findIndex((c) => c.id === 'version');
    if (idx >= 0) {
      checks[idx] = result;
    } else {
      checks.push(result);
    }
    updateChecklist(checks);
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

  // Load settings (fast, file I/O only) then populate form
  invoke('get_settings')
    .then((settings) => {
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
    })
    .catch((e) => console.error('Failed to load settings:', e));

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

  invoke('run_checklist').catch(console.error);

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
