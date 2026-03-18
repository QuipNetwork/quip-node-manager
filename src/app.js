// SPDX-License-Identifier: AGPL-3.0-or-later

// Tauri IPC bridge
const invoke =
  window.__TAURI__?.core?.invoke ??
  (() => Promise.reject('Tauri not available'));
const listen =
  window.__TAURI__?.event?.listen ?? (() => Promise.resolve(() => {}));

// App state
const state = {
  settings: null,
  containerRunning: false,
  checksPassed: false,
  detectedGpus: [], // { index, name }
  logLines: [],
  MAX_LOG_LINES: 500,
  pollInterval: null,
  portPollInterval: null,
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
  const input = document.getElementById('public-host');
  input.disabled = !enabled;
  if (!enabled) input.value = '';
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

// ─── GPU mining toggle & options ──────────────────────────────────────────────
function updateGpuOptionsVisibility() {
  const enabled = document.getElementById('gpu-enable').checked;
  document.getElementById('gpu-options').style.display = enabled ? 'block' : 'none';
}

document.getElementById('gpu-enable').addEventListener('change', updateGpuOptionsVisibility);

document.getElementById('gpu-utilization').addEventListener('input', () => {
  const val = document.getElementById('gpu-utilization').value;
  document.getElementById('gpu-util-display').textContent = `${val}%`;
});

// ─── Collect form → NodeConfig ────────────────────────────────────────────────
function collectConfig() {
  const gpuEnabled = document.getElementById('gpu-enable').checked;
  const gpuBackend = document.getElementById('gpu-backend')?.value || 'local';
  const gpuUtilization = parseInt(document.getElementById('gpu-utilization')?.value) || 80;
  const gpuYielding = document.getElementById('gpu-yielding')?.checked ?? false;

  const qpuApiKey = document.getElementById('qpu-api-key')?.value?.trim() ?? '';
  const qpuConfig = qpuApiKey
    ? {
        api_key: qpuApiKey,
        solver: document.getElementById('qpu-solver')?.value?.trim() ?? '',
        region_url: document.getElementById('qpu-region-url')?.value?.trim() ?? '',
        daily_budget: document.getElementById('qpu-daily-budget')?.value?.trim() ?? '',
      }
    : null;

  const fanoutRaw = document.getElementById('fanout')?.value?.trim();
  const fanout = fanoutRaw ? (parseInt(fanoutRaw) || null) : null;

  return {
    port: parseInt(document.getElementById('port').value) || 20049,
    listen: document.getElementById('listen')?.value?.trim() || '::',
    public_host: document.getElementById('public-host-enable')?.checked
      ? document.getElementById('public-host')?.value?.trim() ?? ''
      : '',
    node_name: document.getElementById('node-name')?.value?.trim() ?? '',
    peers: document
      .getElementById('peers')
      .value.split('\n')
      .map((s) => s.trim())
      .filter((s) => s.length > 0),
    auto_mine: document.getElementById('auto-mine')?.checked ?? false,
    secret: state.settings?.node_config?.secret ?? '',
    num_cpus: parseInt(document.getElementById('num-cpus').value) || 1,
    gpu_backend: gpuEnabled ? gpuBackend : 'local',
    gpu_device_configs: gpuEnabled
      ? [{ index: 0, enabled: true, utilization: gpuUtilization, yielding: gpuYielding }]
      : [],
    qpu_config: qpuConfig,
    timeout: parseInt(document.getElementById('timeout')?.value) || 3,
    heartbeat_interval:
      parseInt(document.getElementById('heartbeat-interval')?.value) || 15,
    heartbeat_timeout:
      parseInt(document.getElementById('heartbeat-timeout')?.value) || 300,
    fanout,
    verify_ssl: document.getElementById('verify-ssl')?.checked ?? true,
    log_level: document.getElementById('log-level')?.value || 'info',
  };
}

// ─── Apply form → settings ────────────────────────────────────────────────────
function applyFormToSettings() {
  if (!state.settings) return;
  state.settings.node_config = collectConfig();
  // cuda image only for NVIDIA CUDA; MPS uses cpu image (Metal requires native sidecar)
  const gpuEnabled = document.getElementById('gpu-enable').checked;
  const gpuBackend = document.getElementById('gpu-backend')?.value;
  state.settings.image_tag =
    gpuEnabled && gpuBackend === 'local' ? 'cuda' : 'cpu';
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
  if (publicHost) {
    document.getElementById('public-host-enable').checked = true;
    document.getElementById('public-host').disabled = false;
    document.getElementById('public-host').value = publicHost;
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
  document.getElementById('verify-ssl').checked = c.verify_ssl ?? true;

  // Auto-expand custom settings if any non-default values are set
  const hasCustom =
    publicHost ||
    (c.peers || []).length > 0 ||
    c.timeout !== 3 ||
    c.heartbeat_interval !== 15 ||
    c.heartbeat_timeout !== 300 ||
    c.fanout != null ||
    c.log_level !== 'info' ||
    !(c.verify_ssl ?? true);
  if (hasCustom) {
    document.getElementById('btn-custom-toggle').setAttribute('aria-expanded', 'true');
    document.getElementById('custom-settings-section').style.display = '';
  }

  // CPU Miner
  document.getElementById('num-cpus').value = c.num_cpus ?? 1;

  // GPU Miner
  const gpuEnabled =
    c.gpu_backend === 'mps' ||
    (c.gpu_device_configs || []).some((d) => d.enabled);
  document.getElementById('gpu-enable').checked = gpuEnabled;
  document.getElementById('gpu-backend').value =
    c.gpu_backend === 'mps' ? 'mps' : 'local';
  const gpuCfg = (c.gpu_device_configs || [])[0];
  const savedUtil = gpuCfg?.utilization ?? 80;
  document.getElementById('gpu-utilization').value = savedUtil;
  document.getElementById('gpu-util-display').textContent = `${savedUtil}%`;
  document.getElementById('gpu-yielding').checked = gpuCfg?.yielding ?? false;
  updateGpuOptionsVisibility();

  // QPU Miner
  const qpu = c.qpu_config;
  if (qpu) {
    document.getElementById('qpu-api-key').value = qpu.api_key ?? '';
    document.getElementById('qpu-solver').value = qpu.solver ?? '';
    document.getElementById('qpu-region-url').value = qpu.region_url ?? '';
    document.getElementById('qpu-daily-budget').value = qpu.daily_budget ?? '';
    if (qpu.api_key) {
      document.getElementById('qpu-section').style.display = 'block';
      document.getElementById('btn-qpu-toggle').textContent =
        'Hide QPU Configuration';
    }
  }

  updateGpuDevicesVisibility();
  // GPU device list rendered after list_gpu_devices call in init
}

// ─── Start/Stop/Apply enable state ───────────────────────────────────────────
function updateStartStopState() {
  document.getElementById('btn-start').disabled =
    !state.checksPassed || state.containerRunning;
  document.getElementById('btn-stop').disabled = !state.containerRunning;
  document.getElementById('btn-apply').disabled = !state.containerRunning;
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
  const TOTAL_CHECKS = 7;
  const allPassed =
    checks.length === TOTAL_CHECKS && checks.every((c) => c.passed);
  const failing = checks.filter((c) => !c.passed).length;

  state.checksPassed = allPassed;

  const summary = document.getElementById('checklist-summary');
  const checklistEl = document.getElementById('checklist');
  const toggleBtn = document.getElementById('checklist-toggle');

  if (summary) {
    if (checks.length < TOTAL_CHECKS) {
      summary.textContent = 'Checking\u2026';
      summary.style.color = 'var(--text-faint)';
    } else if (allPassed) {
      summary.textContent = '\u2713 All requirements met';
      summary.style.color = 'var(--success)';
      // Auto-collapse when all pass
      toggleBtn.setAttribute('aria-expanded', 'false');
      checklistEl.style.display = 'none';
    } else {
      summary.textContent = `\u2717 ${failing} not met`;
      summary.style.color = 'var(--error)';
      // Auto-expand when there are failures
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
    icon.textContent = check.passed ? '\u2713' : '\u2717';
    icon.style.color = check.passed ? 'var(--success)' : 'var(--error)';

    const label = item.querySelector('.check-label');
    if (label && check.label) label.textContent = check.label;

    const actionBtn = item.querySelector('.check-action');
    if (actionBtn && actionBtn.tagName === 'BUTTON') {
      actionBtn.style.display = check.passed ? 'none' : 'inline-flex';
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
    window.open('https://docs.docker.com/get-docker/', '_blank');
  });

document
  .querySelector('[data-id="image"] .check-action')
  ?.addEventListener('click', async () => {
    const tag = state.settings?.image_tag || 'cpu';
    const statusEl = document.getElementById('apply-status');
    statusEl.textContent = 'Pulling image\u2026';
    try {
      await invoke('pull_node_image', { imageTag: tag });
      statusEl.textContent = 'Image pulled.';
      await invoke('run_checklist');
    } catch (e) {
      statusEl.textContent = `Pull failed: ${e}`;
    }
  });

document
  .querySelector('[data-id="secret"] .check-action')
  ?.addEventListener('click', async () => {
    try {
      const secret = await invoke('generate_node_secret');
      document.getElementById('secret-display').value = secret;
      if (state.settings) {
        state.settings.node_config.secret = secret;
        await invoke('update_settings', { settings: state.settings });
      }
      await invoke('run_checklist');
    } catch (e) {
      console.error(e);
    }
  });

// ─── Start / Stop ─────────────────────────────────────────────────────────────
document.getElementById('btn-start').addEventListener('click', async () => {
  applyFormToSettings();
  const applyStatus = document.getElementById('apply-status');
  applyStatus.textContent = 'Starting\u2026';
  try {
    await invoke('update_settings', { settings: state.settings });
    await invoke('start_node_container', {
      config: state.settings.node_config,
      imageTag: state.settings.image_tag,
    });
    applyStatus.textContent = 'Node started.';
    await invoke('start_log_stream');
    await pollStatus();
  } catch (e) {
    applyStatus.textContent = `Error: ${e}`;
  }
});

document.getElementById('btn-stop').addEventListener('click', async () => {
  try {
    await invoke('stop_log_stream');
    await invoke('stop_node_container');
    state.containerRunning = false;
    setStatus('stopped');
    updateStartStopState();
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
    if (state.containerRunning) {
      applyStatus.textContent = 'Restarting\u2026';
      await invoke('stop_log_stream');
      await invoke('stop_node_container');
      await invoke('start_node_container', {
        config: state.settings.node_config,
        imageTag: state.settings.image_tag,
      });
      await invoke('start_log_stream');
    }
    applyStatus.textContent = 'Settings saved.';
    setTimeout(() => {
      applyStatus.textContent = '';
    }, 3000);
  } catch (e) {
    applyStatus.textContent = `Error: ${e}`;
  }
});

// ─── Polling ──────────────────────────────────────────────────────────────────
async function pollStatus() {
  try {
    const status = await invoke('get_container_status');
    state.containerRunning = status.running;
    setStatus(status.running ? 'running' : 'stopped');
  } catch {
    state.containerRunning = false;
    setStatus('stopped');
  }
  updateStartStopState();
}

async function pollPort() {
  try {
    const port = state.settings?.node_config?.port ?? 20049;
    const ip = await invoke('detect_public_ip');
    const ok = await invoke('check_port_forwarding', { ip, port });
    const item = document.querySelector('.checklist-item[data-id="port"]');
    if (item) {
      const icon = item.querySelector('.check-icon');
      icon.textContent = ok ? '\u2713' : '\u2717';
      icon.style.color = ok ? 'var(--success)' : 'var(--error)';
    }
  } catch {
    // ignore
  }
}

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
}

// ─── Initialize ───────────────────────────────────────────────────────────────
async function init() {
  try {
    state.settings = await invoke('get_settings');
    populateForm(state.settings);

    if (state.settings.active_tab && state.settings.active_tab !== 'status') {
      document
        .querySelector(`[data-tab="${state.settings.active_tab}"]`)
        ?.click();
    }
  } catch (e) {
    console.error('Failed to load settings:', e);
  }

  // Auto-detect GPU backend (only if no saved GPU config yet)
  try {
    const noSavedGpu =
      !(state.settings?.node_config?.gpu_device_configs || []).some((d) => d.enabled) &&
      state.settings?.node_config?.gpu_backend !== 'mps';
    if (noSavedGpu) {
      const detectedBackend = await invoke('detect_gpu_backend');
      if (detectedBackend !== 'none') {
        document.getElementById('gpu-backend').value = detectedBackend;
      }
    }
  } catch {
    // ignore
  }

  await setupListeners();
  await pollStatus();
  await invoke('run_checklist').catch(console.error);
  await invoke('start_log_stream').catch(console.error);

  state.pollInterval = setInterval(pollStatus, 10_000);
  state.portPollInterval = setInterval(pollPort, 60_000);
}

init().catch(console.error);
