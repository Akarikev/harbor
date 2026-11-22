// harbor — frontend logic
// Calls Tauri Rust commands via __TAURI_INTERNALS__

const THEME_LABELS = {
  light: 'Light',
  system: 'Follow system',
  dark: 'Dark',
};

const REFRESH_LABELS = {
  2000: 'Every 2 seconds',
  5000: 'Every 5 seconds',
  10000: 'Every 10 seconds',
  30000: 'Every 30 seconds',
  0: 'Off',
};

const KILL_ACTION_LABELS = {
  soft: 'Soft (SIGTERM)',
  force: 'Force (SIGKILL)',
};

function getTauriApi() {
  const api = window.__TAURI__;
  if (!api?.core?.invoke) {
    throw new Error('Tauri invoke API is unavailable');
  }
  return api;
}

function invokeCommand(command, args) {
  return getTauriApi().core.invoke(command, args);
}

function getCurrentAppWindow() {
  const api = getTauriApi();
  return api.webviewWindow?.getCurrentWebviewWindow?.() ?? api.window?.getCurrentWindow?.();
}

// ─── State ───────────────────────────────────────────────────────────────────
let ports = [];
let popupFocused = false;
let refreshInterval = null;
let scanInFlight = false;
let latestScanRequest = 0;
let currentTheme = 'system';
let showAllPorts = false;
let confirmBeforeKill = true;
let defaultKillAction = 'soft';
let expandedPort = null;
let confirmResolver = null;
let toastTimer = null;

// ─── DOM refs ─────────────────────────────────────────────────────────────────
const portList = document.getElementById('port-list');
const emptyState = document.getElementById('empty-state');
const loadingState = document.getElementById('loading-state');
const quitBtn = document.getElementById('quit-btn');
const settingsBtn = document.getElementById('settings-btn');
const showAllToggle = document.getElementById('show-all-toggle');
const showAllToggleSettings = document.getElementById('show-all-toggle-settings');
const confirmKillToggle = document.getElementById('confirm-kill-toggle');
const settingsPanel = document.getElementById('settings-panel');
const settingsClose = document.getElementById('settings-close');
const themeSelect = document.getElementById('theme-select');
const refreshSelect = document.getElementById('refresh-select');
const killActionSelect = document.getElementById('kill-action-select');
const confirmPanel = document.getElementById('confirm-panel');
const confirmMessage = document.getElementById('confirm-message');
const confirmCancel = document.getElementById('confirm-cancel');
const confirmAccept = document.getElementById('confirm-accept');
const toastEl = document.getElementById('toast');

// ─── Helpers ──────────────────────────────────────────────────────────────────
function formatMem(mb) {
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
  if (mb >= 1) return `${mb.toFixed(0)} MB`;
  return `< 1 MB`;
}

function formatCpu(pct) {
  return `${pct.toFixed(0)}%`;
}

function formatUptime(seconds) {
  if (!seconds || seconds <= 0) return '0m';
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);

  const parts = [];
  if (d > 0) parts.push(`${d}d`);
  if (h > 0) parts.push(`${h}h`);
  if (m > 0 || parts.length === 0) parts.push(`${m}m`);

  return parts.slice(0, 2).join(' ');
}

function getEnergyImpact(cpu) {
  if (cpu < 5) return { label: 'Low', class: 'energy-low' };
  if (cpu < 20) return { label: 'Medium', class: 'energy-medium' };
  return { label: 'High', class: 'energy-high' };
}

function energyIconSvg() {
  return `<svg viewBox="0 0 24 24" fill="currentColor" stroke="none" aria-hidden="true"><polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"></polygon></svg>`;
}

function getDisplayName(entry) {
  return entry.process_name?.trim() || getCommandName(entry.cmdline);
}

function getPathContext(cmdline, pid) {
  if (!cmdline) return `Unknown (PID ${pid})`;
  const parts = cmdline.split(' ');
  for (let i = parts.length - 1; i >= 0; i--) {
    const p = parts[i];
    if (p.includes('/') || p.includes('\\')) {
      return p.split(/[\/\\]/).pop() || p;
    }
  }
  return parts[0];
}

function getCommandName(cmdline, processName) {
  const firstToken = String(cmdline || '').trim().split(/\s+/)[0];
  if (firstToken) {
    return firstToken.split(/[\/\\]/).pop() || firstToken;
  }
  return processName || 'unknown';
}

function getDirectoryContext(cwd, cmdline) {
  if (cwd) return cwd;
  const firstToken = String(cmdline || '').trim().split(/\s+/)[0];
  if (!firstToken) return 'Unknown';
  if (!firstToken.includes('/') && !firstToken.includes('\\')) return 'Unknown';
  const normalized = firstToken.replace(/[\/\\][^\/\\]*$/, '');
  return normalized || 'Unknown';
}

function canOpenInBrowser(entry) {
  if (isProtectedEntry(entry)) {
    return false;
  }

  const port = Number(entry.port);
  const processName = String(entry.process_name || '').toLowerCase();
  const cmdline = String(entry.cmdline || '').toLowerCase();

  const blockedNames = [
    'postgres', 'mysql', 'mariadb', 'redis', 'mongo', 'mongodb', 'elasticsearch',
    'memcached', 'ssh', 'nginx', 'apache', 'caddy', 'traefik',
  ];
  if (blockedNames.some((name) => processName.includes(name) || cmdline.includes(name))) {
    return false;
  }

  const webNames = [
    'node', 'vite', 'webpack', 'parcel', 'rollup', 'next', 'nuxt', 'astro',
    'remix', 'express', 'serve', 'http-server', 'live-server', 'python',
    'uvicorn', 'gunicorn', 'flask', 'django', 'php', 'deno', 'bun', 'ruby',
    'rails', 'go', 'dotnet',
  ];
  const commonWebPort =
    [80, 81, 3000, 3001, 3002, 3003, 4200, 5000, 5001, 5173, 5174, 5500, 8000, 8001, 8080, 8081, 8888, 9000, 10000]
      .includes(port) ||
    (port >= 3000 && port <= 3999) ||
    (port >= 8000 && port <= 8100);

  return commonWebPort || webNames.some((name) => processName.includes(name) || cmdline.includes(name));
}

function getKillPids(entry) {
  if (Array.isArray(entry.pids) && entry.pids.length > 0) {
    return entry.pids.filter((pid) => Number(pid) > 0);
  }
  return Number(entry.pid) > 0 ? [Number(entry.pid)] : [];
}

function isProtectedEntry(entry) {
  return Boolean(entry.is_protected) || Number(entry.pid) <= 0;
}

function getSavedTheme() {
  return localStorage.getItem('harbor-theme') || 'system';
}

function getSavedRefreshInterval() {
  return Number(localStorage.getItem('harbor-refresh-interval') || '5000');
}

function getSavedShowAll() {
  return localStorage.getItem('harbor-show-all') === 'true';
}

function getSavedConfirmBeforeKill() {
  const saved = localStorage.getItem('harbor-confirm-kill');
  return saved === null ? true : saved === 'true';
}

function getSavedKillAction() {
  const saved = localStorage.getItem('harbor-kill-action');
  return saved === 'force' ? 'force' : 'soft';
}

function syncColorScheme(theme) {
  const root = document.documentElement;
  if (theme === 'dark') {
    root.style.colorScheme = 'dark';
  } else if (theme === 'light') {
    root.style.colorScheme = 'light';
  } else {
    root.style.colorScheme = window.matchMedia('(prefers-color-scheme: dark)').matches
      ? 'dark'
      : 'light';
  }
}

function setCustomSelectValue(selectEl, value, labels) {
  if (!selectEl) return;
  selectEl.dataset.value = String(value);
  const label = labels[value] ?? labels[String(value)] ?? String(value);
  const valueEl = selectEl.querySelector('.custom-select-value');
  if (valueEl) valueEl.textContent = label;

  for (const option of selectEl.querySelectorAll('.custom-select-option')) {
    option.classList.toggle('selected', option.dataset.value === String(value));
  }
}

function closeAllCustomSelects(except = null) {
  for (const select of document.querySelectorAll('.custom-select.open')) {
    if (select === except) continue;
    select.classList.remove('open');
    select.querySelector('.custom-select-menu')?.classList.add('hidden');
  }
}

function initCustomSelect(selectEl, labels, onChange) {
  if (!selectEl) return;

  const trigger = selectEl.querySelector('.custom-select-trigger');
  const menu = selectEl.querySelector('.custom-select-menu');
  if (!trigger || !menu) return;

  trigger.addEventListener('click', (event) => {
    event.stopPropagation();
    const willOpen = !selectEl.classList.contains('open');
    closeAllCustomSelects(selectEl);
    selectEl.classList.toggle('open', willOpen);
    menu.classList.toggle('hidden', !willOpen);
  });

  for (const option of selectEl.querySelectorAll('.custom-select-option')) {
    option.addEventListener('click', (event) => {
      event.stopPropagation();
      const value = option.dataset.value;
      setCustomSelectValue(selectEl, value, labels);
      closeAllCustomSelects();
      onChange(value);
    });
  }
}

function applyTheme(theme) {
  currentTheme = theme;
  document.documentElement.setAttribute('data-theme', theme);
  localStorage.setItem('harbor-theme', theme);
  syncColorScheme(theme);
  setCustomSelectValue(themeSelect, theme, THEME_LABELS);
}

function restartRefreshInterval(ms) {
  clearInterval(refreshInterval);
  refreshInterval = popupFocused && ms > 0 ? setInterval(refresh, ms) : null;
}

function applyRefreshInterval(ms) {
  const parsed = Number(ms);
  localStorage.setItem('harbor-refresh-interval', String(parsed));
  restartRefreshInterval(parsed);
  setCustomSelectValue(refreshSelect, parsed, REFRESH_LABELS);
}

function applyShowAll(enabled) {
  showAllPorts = Boolean(enabled);
  localStorage.setItem('harbor-show-all', String(showAllPorts));
  if (showAllToggle) showAllToggle.checked = showAllPorts;
  if (showAllToggleSettings) showAllToggleSettings.checked = showAllPorts;
}

function applyConfirmBeforeKill(enabled) {
  confirmBeforeKill = Boolean(enabled);
  localStorage.setItem('harbor-confirm-kill', String(confirmBeforeKill));
  if (confirmKillToggle) confirmKillToggle.checked = confirmBeforeKill;
}

function applyKillAction(action) {
  defaultKillAction = action === 'force' ? 'force' : 'soft';
  localStorage.setItem('harbor-kill-action', defaultKillAction);
  setCustomSelectValue(killActionSelect, defaultKillAction, KILL_ACTION_LABELS);
}

function openSettings() {
  settingsPanel.classList.remove('hidden');
  settingsPanel.setAttribute('aria-hidden', 'false');
}

function closeSettings() {
  closeAllCustomSelects();
  settingsPanel.classList.add('hidden');
  settingsPanel.setAttribute('aria-hidden', 'true');
}

function showToast(message, tone = 'error') {
  if (!toastEl) return;
  toastEl.textContent = message;
  toastEl.dataset.tone = tone;
  toastEl.classList.remove('hidden');
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    toastEl.classList.add('hidden');
  }, 3200);
}

function showConfirm({ title, message, confirmText = 'Confirm', confirmTone = 'danger' }) {
  confirmPanel.classList.remove('hidden');
  confirmPanel.setAttribute('aria-hidden', 'false');
  document.getElementById('confirm-title').textContent = title;
  confirmMessage.textContent = message;
  confirmAccept.textContent = confirmText;
  confirmAccept.dataset.tone = confirmTone;

  return new Promise((resolve) => {
    confirmResolver = resolve;
  });
}

function closeConfirm(result = false) {
  confirmPanel.classList.add('hidden');
  confirmPanel.setAttribute('aria-hidden', 'true');
  if (confirmResolver) {
    confirmResolver(result);
    confirmResolver = null;
  }
}

async function killEntry(entryRef, { force = false } = {}) {
  const entry = ports.find((p) => p.port === entryRef.port) || entryRef;

  if (isProtectedEntry(entry)) {
    showToast('This process is protected and cannot be killed.');
    return;
  }

  const killPids = getKillPids(entry);
  if (killPids.length === 0) {
    showToast('No process ID available to kill.');
    return;
  }

  const useForce = force || defaultKillAction === 'force';
  const signalLabel = useForce ? 'SIGKILL' : 'SIGTERM';

  if (confirmBeforeKill) {
    const confirmed = await showConfirm({
      title: useForce ? 'Force kill process?' : 'Kill process?',
      message: useForce
        ? `Send ${signalLabel} to :${entry.port} (${entry.process_name || 'unknown'})? This stops it immediately.`
        : `Send ${signalLabel} to :${entry.port} (${entry.process_name || 'unknown'})?`,
      confirmText: useForce ? 'Force kill' : 'Kill',
      confirmTone: useForce ? 'warn' : 'danger',
    });
    if (!confirmed) return;
  }

  try {
    await invokeCommand('kill_processes', { port: entry.port, pids: killPids, force: useForce });
    const row = portList.querySelector(`.port-row[data-port="${entry.port}"]`);
    if (row) removeRow(entry.port, row);
    refresh({ supersede: true });
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    console.error('kill_processes failed:', err);
    showToast(message || 'Failed to kill process.');
  }
}

// ─── Port scanning ────────────────────────────────────────────────────────────
async function refresh({ supersede = false } = {}) {
  if (!popupFocused || (scanInFlight && !supersede)) return;
  const requestId = ++latestScanRequest;
  if (scanInFlight) return;

  scanInFlight = true;
  if (ports.length === 0) {
    loadingState.classList.remove('hidden');
    emptyState.classList.add('hidden');
  }

  try {
    const result = await invokeCommand('scan_ports', { showAll: showAllPorts });
    if (requestId !== latestScanRequest) return;
    ports = Array.isArray(result) ? result : [];
    render();
  } catch (err) {
    if (requestId !== latestScanRequest) return;
    console.error('scan_ports failed:', err);
    emptyState.classList.remove('hidden');
    document.getElementById('empty-title').textContent =
      'Error scanning ports: ' + (err instanceof Error ? err.message : String(err));
  } finally {
    scanInFlight = false;
    loadingState.classList.add('hidden');
    if (requestId !== latestScanRequest) refresh();
  }
}

// ─── Render ───────────────────────────────────────────────────────────────────
function render() {
  loadingState.classList.add('hidden');

  if (ports.length === 0) {
    portList.innerHTML = '';
    emptyState.classList.remove('hidden');
    return;
  }

  emptyState.classList.add('hidden');
  if (expandedPort !== null && !ports.some((port) => port.port === expandedPort)) {
    expandedPort = null;
  }

  const existingRows = new Map(
    [...portList.querySelectorAll('.port-row')].map((el) => [Number(el.dataset.port), el])
  );

  const newPorts = new Set(ports.map((p) => p.port));

  for (const [port, el] of existingRows) {
    if (!newPorts.has(port)) {
      el.style.opacity = '0';
      el.style.transition = 'opacity 0.2s';
      setTimeout(() => el.remove(), 200);
    }
  }

  for (let i = 0; i < ports.length; i++) {
    const entry = ports[i];
    const existing = existingRows.get(entry.port);

    if (existing) {
      const wasProtected = existing.classList.contains('protected');
      const nowProtected = isProtectedEntry(entry);
      const pidChanged = Number(existing.dataset.pid) !== Number(entry.pid);

      // Recreate when protection or PID identity changes so actions stay correct.
      if (wasProtected !== nowProtected || pidChanged) {
        existing.replaceWith(createRow(entry));
        continue;
      }

      const energy = getEnergyImpact(entry.cpu_percent);
      existing.dataset.pid = entry.pid;

      for (const value of existing.querySelectorAll('.cpu-val')) {
        value.textContent = formatCpu(entry.cpu_percent);
      }
      for (const value of existing.querySelectorAll('.mem-val')) {
        value.textContent = formatMem(entry.mem_mb);
      }
      for (const value of existing.querySelectorAll('.uptime-val')) {
        value.textContent = formatUptime(entry.uptime_seconds);
      }

      const energyEl = existing.querySelector('.energy-val');
      const energyPill = existing.querySelector('.status-pill');
      if (energyEl) {
        energyEl.textContent = energy.label;
      }
      if (energyPill) {
        energyPill.className = `status-pill ${energy.class}`;
      }
      const detailEnergy = existing.querySelector('.detail-energy');
      if (detailEnergy) {
        detailEnergy.className = `detail-value detail-energy ${energy.class}`;
        detailEnergy.textContent = `${energy.label} impact`;
      }

      const nameEl = existing.querySelector('.process-name');
      if (nameEl) {
        nameEl.textContent = getDisplayName(entry);
      }

      syncRowExpansion(existing, entry.port === expandedPort);
      portList.appendChild(existing);
    } else {
      const row = createRow(entry);
      portList.appendChild(row);
    }
  }
}

function createRow(entry) {
  const row = document.createElement('div');
  row.className = 'port-row';
  row.dataset.port = entry.port;
  row.dataset.pid = entry.pid;

  const displayName = getDisplayName(entry);
  const pathContext = getPathContext(entry.cmdline, entry.pid);
  const commandName = getCommandName(entry.cmdline, displayName);
  const directoryContext = getDirectoryContext(entry.cwd, entry.cmdline);
  const energy = getEnergyImpact(entry.cpu_percent);
  const isExpanded = expandedPort === entry.port;
  const openable = canOpenInBrowser(entry);
  const protectedEntry = isProtectedEntry(entry);

  if (protectedEntry) {
    row.classList.add('protected');
  }

  row.innerHTML = `
    <div class="row-top">
      <div class="row-left">:${entry.port}</div>
      <div class="row-main">
        <div class="process-name">${escapeHtml(displayName)}</div>
        <div class="process-meta">
          <div class="meta-item process-path" title="${escapeHtml(entry.cmdline)}">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"></path></svg>
            <span>${escapeHtml(pathContext)}</span>
          </div>
          <div class="meta-item" title="Uptime">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor"><circle cx="12" cy="12" r="10"></circle><polyline points="12 6 12 12 16 14"></polyline></svg>
            <span class="uptime-val">${formatUptime(entry.uptime_seconds)}</span>
          </div>
          <div class="meta-item" title="CPU Usage">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor"><rect x="4" y="4" width="16" height="16" rx="2" ry="2"></rect><rect x="9" y="9" width="6" height="6"></rect><line x1="9" y1="1" x2="9" y2="4"></line><line x1="15" y1="1" x2="15" y2="4"></line><line x1="9" y1="20" x2="9" y2="23"></line><line x1="15" y1="20" x2="15" y2="23"></line><line x1="20" y1="9" x2="23" y2="9"></line><line x1="20" y1="14" x2="23" y2="14"></line><line x1="1" y1="9" x2="4" y2="9"></line><line x1="1" y1="14" x2="4" y2="14"></line></svg>
            <span class="cpu-val">${formatCpu(entry.cpu_percent)}</span>
          </div>
          <div class="meta-item" title="Memory">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor"><rect x="2" y="2" width="20" height="8" rx="2" ry="2"></rect><rect x="2" y="14" width="20" height="8" rx="2" ry="2"></rect><line x1="6" y1="6" x2="6.01" y2="6"></line><line x1="6" y1="18" x2="6.01" y2="18"></line></svg>
            <span class="mem-val">${formatMem(entry.mem_mb)}</span>
          </div>
        </div>
      </div>
      <div class="row-right">
        <div class="status-pill ${energy.class}" title="Energy Impact">
          ${energyIconSvg()}
          <span class="energy-val">${energy.label}</span>
        </div>
        <div class="hover-actions">
          ${openable ? `
          <button class="hover-action btn-open" type="button" title="Open in browser" aria-label="Open in browser">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"></circle><line x1="2" y1="12" x2="22" y2="12"></line><path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"></path></svg>
          </button>` : ''}
          <button class="hover-action btn-copy" type="button" title="Copy port" aria-label="Copy port">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path></svg>
          </button>
          ${protectedEntry ? `
          <span class="system-badge" title="System process — cannot kill">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="11" width="18" height="11" rx="2" ry="2"></rect><path d="M7 11V7a5 5 0 0 1 10 0v4"></path></svg>
          </span>` : `
          <button class="hover-action kill" type="button" title="Kill process" aria-label="Kill process">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"></polyline><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path></svg>
          </button>`}
        </div>
        <button class="expand-btn" type="button" aria-label="${isExpanded ? 'Collapse details' : 'Expand details'}">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="9 18 15 12 9 6"></polyline></svg>
        </button>
      </div>
    </div>
    <div class="row-details">
      <div class="detail-grid">
        <div class="detail-item">
          <span class="detail-label">PID</span>
          <span class="detail-value">${entry.pid || '—'}</span>
        </div>
        <div class="detail-item">
          <span class="detail-label">Command</span>
          <span class="detail-value" title="${escapeHtml(entry.cmdline)}">${escapeHtml(commandName)}</span>
        </div>
        <div class="detail-item detail-wide">
          <span class="detail-label">Directory</span>
          <span class="detail-value" title="${escapeHtml(directoryContext)}">${escapeHtml(directoryContext)}</span>
        </div>
        <div class="detail-item">
          <span class="detail-label">Port</span>
          <span class="detail-value">:${entry.port}</span>
        </div>
        <div class="detail-item">
          <span class="detail-label">Uptime</span>
          <span class="detail-value uptime-val">${formatUptime(entry.uptime_seconds)}</span>
        </div>
        <div class="detail-item">
          <span class="detail-label">CPU</span>
          <span class="detail-value cpu-val">${formatCpu(entry.cpu_percent)}</span>
        </div>
        <div class="detail-item">
          <span class="detail-label">Memory</span>
          <span class="detail-value mem-val">${formatMem(entry.mem_mb)}</span>
        </div>
        <div class="detail-item">
          <span class="detail-label">Energy</span>
          <span class="detail-value detail-energy ${energy.class}">${energy.label} impact</span>
        </div>
      </div>
      <div class="detail-actions">
        ${openable ? `
        <button class="detail-action-chip btn-open" type="button" title="Open in browser" aria-label="Open in browser">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"></path><polyline points="15 3 21 3 21 9"></polyline><line x1="10" y1="14" x2="21" y2="3"></line></svg>
          <span>Open</span>
        </button>` : ''}
        <button class="detail-action-chip btn-copy" type="button" title="Copy port" aria-label="Copy port">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path></svg>
          <span>Copy</span>
        </button>
        ${protectedEntry ? `
        <span class="detail-protected">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="11" width="18" height="11" rx="2" ry="2"></rect><path d="M7 11V7a5 5 0 0 1 10 0v4"></path></svg>
          System process
        </span>` : `
        <button class="detail-action-chip kill" type="button" title="Kill process" aria-label="Kill process">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"></circle><line x1="15" y1="9" x2="9" y2="15"></line><line x1="9" y1="9" x2="15" y2="15"></line></svg>
          <span>Kill</span>
        </button>
        <button class="detail-action-chip force" type="button" title="Force kill process" aria-label="Force kill process">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M13 2 3 14h7l-1 8 10-12h-7l1-8z"></path></svg>
          <span>Force</span>
        </button>`}
      </div>
    </div>
  `;

  syncRowExpansion(row, isExpanded);

  row.addEventListener('click', (event) => {
    if (event.target.closest('button, a, input, label, textarea, select, .hover-actions, .system-badge')) {
      return;
    }
    expandedPort = expandedPort === entry.port ? null : entry.port;
    render();
  });

  row.querySelector('.expand-btn').addEventListener('click', (event) => {
    event.stopPropagation();
    expandedPort = expandedPort === entry.port ? null : entry.port;
    render();
  });

  bindActionButtons(row, entry);

  return row;
}

function bindActionButtons(row, entry) {
  for (const button of row.querySelectorAll('.btn-open')) {
    button.addEventListener('click', async (event) => {
      event.stopPropagation();
      if (isProtectedEntry(entry)) {
        showToast('This process is protected and cannot be opened.');
        return;
      }
      try {
        await invokeCommand('open_in_browser', { url: `http://localhost:${entry.port}` });
      } catch (_) {}
    });
  }

  for (const button of row.querySelectorAll('.btn-copy')) {
    button.addEventListener('click', async (event) => {
      event.stopPropagation();
      try {
        await navigator.clipboard.writeText(entry.port.toString());
        showToast(`Copied :${entry.port}`, 'success');
      } catch (_) {
        showToast('Failed to copy port.');
      }
    });
  }

  for (const button of row.querySelectorAll('.kill')) {
    button.addEventListener('click', async (event) => {
      event.stopPropagation();
      await killEntry(entry, { force: false });
    });
  }

  for (const button of row.querySelectorAll('.force')) {
    button.addEventListener('click', async (event) => {
      event.stopPropagation();
      await killEntry(entry, { force: true });
    });
  }
}

function syncRowExpansion(row, isExpanded) {
  row.classList.toggle('expanded', isExpanded);
  const details = row.querySelector('.row-details');
  const expandButton = row.querySelector('.expand-btn');
  if (details) {
    details.style.display = isExpanded ? 'block' : 'none';
  }
  if (expandButton) {
    expandButton.setAttribute('aria-label', isExpanded ? 'Collapse details' : 'Expand details');
  }
}

function removeRow(port, row) {
  row.style.opacity = '0';
  row.style.transform = 'translateX(8px)';
  row.style.transition = 'opacity 0.25s, transform 0.25s';
  setTimeout(() => row.remove(), 250);
  ports = ports.filter((p) => p.port !== port);
  if (expandedPort === port) {
    expandedPort = null;
  }
  if (ports.length === 0) emptyState.classList.remove('hidden');
}

function escapeHtml(str) {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

// ─── Events ───────────────────────────────────────────────────────────────────
showAllToggle.addEventListener('change', () => {
  applyShowAll(showAllToggle.checked);
  refresh({ supersede: true });
});

showAllToggleSettings.addEventListener('change', () => {
  applyShowAll(showAllToggleSettings.checked);
  refresh({ supersede: true });
});

confirmKillToggle.addEventListener('change', () => {
  applyConfirmBeforeKill(confirmKillToggle.checked);
});

settingsBtn.addEventListener('click', () => {
  const isOpen = !settingsPanel.classList.contains('hidden');
  if (isOpen) {
    closeSettings();
  } else {
    openSettings();
  }
});

settingsClose.addEventListener('click', closeSettings);

settingsPanel.addEventListener('click', (event) => {
  if (event.target === settingsPanel || event.target === document.getElementById('settings-backdrop')) {
    closeSettings();
  }
});

confirmCancel.addEventListener('click', () => closeConfirm(false));
confirmAccept.addEventListener('click', () => closeConfirm(true));
confirmPanel.addEventListener('click', (event) => {
  if (event.target === confirmPanel || event.target === document.getElementById('confirm-backdrop')) {
    closeConfirm(false);
  }
});

document.addEventListener('click', () => closeAllCustomSelects());

initCustomSelect(themeSelect, THEME_LABELS, (value) => applyTheme(value));
initCustomSelect(refreshSelect, REFRESH_LABELS, (value) => applyRefreshInterval(value));
initCustomSelect(killActionSelect, KILL_ACTION_LABELS, (value) => applyKillAction(value));

quitBtn.addEventListener('click', async () => {
  try {
    await invokeCommand('quit_app');
  } catch (e) {
    console.error(e);
  }
});

async function hidePopup() {
  try {
    await getCurrentAppWindow()?.hide();
  } catch (err) {
    console.error('window hide failed:', err);
  }
}

function handlePopupFocus(focused) {
  if (popupFocused === focused) return;
  popupFocused = focused;
  restartRefreshInterval(getSavedRefreshInterval());

  if (focused) {
    refresh({ supersede: true });
    return;
  }

  ++latestScanRequest;
}

window.addEventListener('keydown', (event) => {
  if (event.key !== 'Escape') return;
  if (settingsPanel.classList.contains('hidden') && confirmPanel.classList.contains('hidden')) {
    hidePopup();
    return;
  }
  closeSettings();
  closeConfirm(false);
});

window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
  if (currentTheme === 'system') syncColorScheme('system');
});

// ─── Init ─────────────────────────────────────────────────────────────────────
async function init() {
  applyTheme(getSavedTheme());
  applyRefreshInterval(getSavedRefreshInterval());
  applyShowAll(getSavedShowAll());
  applyConfirmBeforeKill(getSavedConfirmBeforeKill());
  applyKillAction(getSavedKillAction());
  const appWindow = getCurrentAppWindow();
  let focusEventCount = 0;
  await appWindow.onFocusChanged(({ payload }) => {
    ++focusEventCount;
    handlePopupFocus(payload);
  });
  const focusEventCountAtQuery = focusEventCount;
  const focused = await appWindow.isFocused();
  if (focusEventCountAtQuery === focusEventCount) handlePopupFocus(focused);
}

init().catch((err) => console.error('window initialization failed:', err));
