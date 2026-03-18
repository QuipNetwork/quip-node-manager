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

// ─── GPU utilization slider ──────────────────────────────────────────────────
const gpuUtilSlider = document.getElementById('gpu-util');
const gpuUtilVal = document.getElementById('gpu-util-val');
gpuUtilSlider.addEventListener('input', () => {
  gpuUtilVal.textContent = gpuUtilSlider.value;
});

// ─── QPU table management ────────────────────────────────────────────────────
function renderQpuTable(qpuConfigs) {
  const tbody = document.getElementById('qpu-tbody');
  tbody.innerHTML = '';
  (qpuConfigs || []).forEach((qpu, i) => {
    const tr = document.createElement('tr');
    tr.innerHTML = `
      <td><input type="text" value="${escHtml(qpu.url)}" data-qpu="${i}" data-field="url" placeholder="https://..." /></td>
      <td><input type="password" value="${escHtml(qpu.api_key)}" data-qpu="${i}" data-field="api_key" placeholder="API key" /></td>
      <td><button class="btn btn-sm btn-danger" data-remove="${i}">&times;</button></td>
    `;
    tbody.appendChild(tr);
  });
}

document.getElementById('btn-add-qpu').addEventListener('click', () => {
  if (!state.settings) return;
  state.settings.node_config.qpu_configs.push({ url: '', api_key: '' });
  renderQpuTable(state.settings.node_config.qpu_configs);
});

document.getElementById('qpu-tbody').addEventListener('click', (e) => {
  const removeIdx = e.target.dataset.remove;
  if (removeIdx !== undefined && state.settings) {
    state.settings.node_config.qpu_configs.splice(Number(removeIdx), 1);
    renderQpuTable(state.settings.node_config.qpu_configs);
  }
});

// ─── Collect form → NodeConfig ────────────────────────────────────────────────
function collectConfig() {
  const gpuBackendMap = { local: 'local', modal: 'modal', mps: 'mps' };
  const gpuDevicesRaw = document.getElementById('gpu-devices').value;
  const gpuDevices = gpuDevicesRaw
    .split(',')
    .map((s) => parseInt(s.trim()))
    .filter((n) => !isNaN(n));

  // Collect QPU rows from DOM
  const qpuRows = document.querySelectorAll('#qpu-tbody tr');
  const qpuConfigs = Array.from(qpuRows).map((row) => ({
    url: row.querySelector('[data-field="url"]')?.value ?? '',
    api_key: row.querySelector('[data-field="api_key"]')?.value ?? '',
  }));

  return {
    num_cpus: parseInt(document.getElementById('num-cpus').value) || 2,
    gpu_backend:
      gpuBackendMap[document.getElementById('gpu-backend').value] || 'local',
    gpu_devices: gpuDevices,
    gpu_utilization: parseInt(document.getElementById('gpu-util').value) || 80,
    qpu_configs: qpuConfigs,
    peers: state.settings?.node_config?.peers ?? [],
    port: state.settings?.node_config?.port ?? 20049,
    secret: state.settings?.node_config?.secret ?? '',
  };
}

// ─── Apply form → settings ────────────────────────────────────────────────────
function applyFormToSettings() {
  if (!state.settings) return;
  state.settings.node_config = collectConfig();
}

// ─── Populate form from settings ─────────────────────────────────────────────
function populateForm(settings) {
  const c = settings.node_config;
  document.getElementById('num-cpus').value = c.num_cpus ?? 2;
  const gpuMap = { local: 'local', modal: 'modal', mps: 'mps' };
  document.getElementById('gpu-backend').value =
    gpuMap[c.gpu_backend] || 'local';
  document.getElementById('gpu-devices').value = (c.gpu_devices || []).join(
    ', '
  );
  document.getElementById('gpu-util').value = c.gpu_utilization ?? 80;
  gpuUtilVal.textContent = c.gpu_utilization ?? 80;
  renderQpuTable(c.qpu_configs || []);
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
    sub.textContent = 'Node container is running';
  } else if (stateStr === 'degraded') {
    dot.classList.add('status-degraded', 'active');
    text.classList.add('status-degraded');
    text.textContent = 'DEGRADED';
    sub.textContent = 'Running but some checks failing';
  } else {
    dot.classList.add('status-stopped');
    text.classList.add('status-stopped');
    text.textContent = 'STOPPED';
    sub.textContent = 'Container not running';
  }
}

// ─── Checklist update ─────────────────────────────────────────────────────────
function updateChecklist(checks) {
  checks.forEach((check) => {
    const item = document.querySelector(
      `.checklist-item[data-id="${check.id}"]`
    );
    if (!item) return;
    const icon = item.querySelector('.check-icon');
    icon.textContent = check.passed ? '\u2713' : '\u2717';
    icon.style.color = check.passed ? 'var(--success)' : 'var(--error)';
    const actionBtn = item.querySelector('.check-action');
    if (actionBtn && actionBtn.tagName === 'BUTTON') {
      actionBtn.style.display = check.passed ? 'none' : 'inline-flex';
    }
  });
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
  // Keep scroll at bottom if already near bottom
  if (output.scrollHeight - output.scrollTop - output.clientHeight < 60) {
    output.scrollTop = output.scrollHeight;
  }
  // Trim DOM nodes
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
      if (state.settings) {
        state.settings.node_config.secret = secret;
        await invoke('update_settings', { settings: state.settings });
      }
      await invoke('run_checklist');
    } catch (e) {
      console.error(e);
    }
  });

document
  .querySelector('[data-id="config"] .check-action')
  ?.addEventListener('click', async () => {
    try {
      if (state.settings) {
        await invoke('generate_config_toml', {
          config: state.settings.node_config,
        });
        await invoke('run_checklist');
      }
    } catch (e) {
      console.error(e);
    }
  });

// ─── Start / Stop ─────────────────────────────────────────────────────────────
document
  .getElementById('btn-start')
  .addEventListener('click', async () => {
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

document
  .getElementById('btn-stop')
  .addEventListener('click', async () => {
    try {
      await invoke('stop_log_stream');
      await invoke('stop_node_container');
      setStatus('stopped');
      state.containerRunning = false;
    } catch (e) {
      console.error(e);
    }
  });

// ─── Apply & Restart ──────────────────────────────────────────────────────────
document
  .getElementById('btn-apply')
  .addEventListener('click', async () => {
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
    if (status.running) {
      setStatus('running');
    } else {
      setStatus('stopped');
    }
  } catch {
    setStatus('stopped');
  }
}

async function pollPort() {
  try {
    const ip = await invoke('detect_public_ip');
    const ok = await invoke('check_port_forwarding', { ip, port: 20049 });
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
    setStatus(s?.toLowerCase() || 'stopped');
  });
}

// ─── Initialize ───────────────────────────────────────────────────────────────
async function init() {
  try {
    state.settings = await invoke('get_settings');
    populateForm(state.settings);

    // Restore active tab
    if (state.settings.active_tab && state.settings.active_tab !== 'status') {
      document
        .querySelector(`[data-tab="${state.settings.active_tab}"]`)
        ?.click();
    }
  } catch (e) {
    console.error('Failed to load settings:', e);
  }

  await setupListeners();
  await pollStatus();
  await invoke('run_checklist').catch(console.error);
  await invoke('start_log_stream').catch(console.error);

  // Poll every 10s for status
  state.pollInterval = setInterval(pollStatus, 10_000);
  // Poll port every 60s
  state.portPollInterval = setInterval(pollPort, 60_000);
}

init().catch(console.error);
