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

function getDisplayName(entry) {
  return entry.process_name?.trim() || getCommandName(entry.cmdline);
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

const byId = (id) => document.getElementById(id);
const viewport = byId('viewport');
const portList = byId('port-list');
const portsView = byId('ports-view');
const settingsView = byId('settings-view');
// WebKitGTK's software rendering keeps a user-scrolled offset after the content shrinks
// below it (e.g. Show all turned off at the bottom), leaving the list stuck off-screen.
const scrollClamp = new ResizeObserver(() => {
  const max = Math.max(0, viewport.scrollHeight - viewport.clientHeight);
  if (viewport.scrollTop > max) viewport.scrollTop = max;
});
scrollClamp.observe(portsView);
scrollClamp.observe(settingsView);
const emptyState = byId('empty-state');
const loadingState = byId('loading-state');
const settingsBtn = byId('settings-btn');
const settingsBack = byId('settings-back');
const showAllToggle = byId('show-all-toggle');
const showAllSettings = byId('show-all-settings');
const refreshSelect = byId('refresh-select');
const confirmKillToggle = byId('confirm-kill-toggle');
const killActionSelect = byId('kill-action-select');
const themeSelect = byId('theme-select');
const toast = byId('toast');
const systemTheme = window.matchMedia('(prefers-color-scheme: dark)');

let ports = [];
let popupFocused = false;
let refreshInterval = null;
let scanInFlight = false;
let latestScanRequest = 0;
let expandedPort = null;
let pendingKill = null;
const killing = new Set();
const copyTimers = new WeakMap();
let toastTimer;
let listScroll = 0;
let resizeFrame = 0;
let requestedHeight = 0;

const icons = {
  uptime: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
  cpu: '<rect x="5" y="5" width="14" height="14" rx="2"/><path d="M9 2v3m6-3v3M9 19v3m6-3v3M2 9h3m-3 6h3m14-6h3m-3 6h3M9 9h6v6H9z"/>',
  memory: '<rect x="3" y="6" width="18" height="12" rx="2"/><path d="M6 18v3m4-3v3m4-3v3m4-3v3M7 10v4m5-4v4m5-4v4"/>',
  bolt: '<path d="m13 2-8 12h6l-1 8 9-12h-6z"/>',
  open: '<path d="M13 4h7v7m0-7-9 9m-2-8H6a2 2 0 0 0-2 2v11a2 2 0 0 0 2 2h11a2 2 0 0 0 2-2v-3"/>',
  copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/>',
  check: '<path d="m5 12 4 4L19 6"/>',
  close: '<path d="m6 6 12 12m0-12L6 18"/>',
  chevron: '<path d="m9 5 7 7-7 7"/>',
  lock: '<rect x="5" y="10" width="14" height="11" rx="2"/><path d="M8 10V7a4 4 0 0 1 8 0v3"/>',
};
const icon = (name) => `<svg viewBox="0 0 24 24" aria-hidden="true">${icons[name]}</svg>`;

function applyPopupHeight() {
  const content = settingsView.hidden ? portsView : settingsView;
  const height = Math.ceil(content.getBoundingClientRect().height
    + byId('header').offsetHeight + byId('footer').offsetHeight
    + window.innerHeight - byId('app').clientHeight);
  if (height === requestedHeight) return Promise.resolve();
  requestedHeight = height;
  return invokeCommand('resize_popup', { height }).catch((err) => {
    requestedHeight = 0;
    console.error('resize_popup failed:', err);
  });
}

function sizePopup() {
  if (resizeFrame) return;
  resizeFrame = requestAnimationFrame(() => {
    resizeFrame = 0;
    applyPopupHeight();
  });
}

// A freshly (re)created window stays unmapped until its first list is rendered
// at its final height, so it appears once, complete, instead of growing on screen.
let firstContentReady = false;
async function markFirstContentReady() {
  if (firstContentReady) return;
  firstContentReady = true;
  await applyPopupHeight();
  // Not requestAnimationFrame: WebKit pauses it while the window is unmapped.
  setTimeout(() => {
    invokeCommand('popup_ready').catch((err) => console.error('popup_ready failed:', err));
  }, 0);
}

function showSettings(open) {
  setPendingKill(null);
  if (open) listScroll = viewport.scrollTop;
  portsView.hidden = open;
  settingsView.hidden = !open;
  settingsBtn.hidden = open;
  settingsBack.hidden = !open;
  byId('quick-show-all').hidden = open;
  byId('app-title').textContent = open ? 'Settings' : 'harbor';
  (open ? settingsBack : settingsBtn).focus({ preventScroll: true });
  viewport.scrollTop = open ? 0 : listScroll;
  sizePopup();
}

function showToast(message, tone = 'error') {
  toast.textContent = message;
  toast.dataset.tone = tone;
  toast.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { toast.hidden = true; }, 3200);
}

function restartRefreshInterval() {
  clearInterval(refreshInterval);
  const ms = Number(refreshSelect.value);
  refreshInterval = popupFocused && ms > 0 ? setInterval(refresh, ms) : null;
}

async function refresh({ supersede = false } = {}) {
  // The first scan runs before the window is shown (and focused); later ones only while open.
  if ((!popupFocused && firstContentReady) || (scanInFlight && !supersede)) return;
  const requestId = ++latestScanRequest;
  if (scanInFlight) return;

  scanInFlight = true;
  if (ports.length === 0) {
    loadingState.hidden = false;
    emptyState.hidden = true;
    sizePopup();
  }

  try {
    const result = await invokeCommand('scan_ports', { showAll: showAllToggle.checked });
    if (requestId !== latestScanRequest) return;
    ports = Array.isArray(result) ? result : [];
    render();
  } catch (err) {
    if (requestId !== latestScanRequest) return;
    const message = `Error scanning ports: ${err instanceof Error ? err.message : String(err)}`;
    if (ports.length === 0) {
      emptyState.textContent = message;
      emptyState.hidden = false;
    } else {
      showToast(message);
    }
  } finally {
    scanInFlight = false;
    loadingState.hidden = true;
    if (firstContentReady) sizePopup();
    else markFirstContentReady();
    if (requestId !== latestScanRequest) refresh();
  }
}

function button(action, label, graphic, style = '') {
  return `<button class="action ${style}" type="button" data-action="${action}" title="${label}" aria-label="${label}">${graphic}</button>`;
}

const rowLayoutTargets = '.meta, .meta-item, .action-group';
const rowLayoutObserver = new ResizeObserver((entries) => {
  const rows = new Set(entries.map(({ target }) => target.closest('.port-row')));
  for (const row of rows) {
    const group = row.querySelector('.action-group:not([hidden])');
    const groupWidth = group.getBoundingClientRect().width;
    if (!groupWidth) continue;

    const width = `${Math.ceil(groupWidth)}px`;
    if (row.style.getPropertyValue('--actions-width') !== width) {
      row.style.setProperty('--actions-width', width);
    }
    const meta = row.querySelector('.meta');
    if (meta.clientWidth) fitMeta(meta, row.classList.contains('confirming'));
  }
});

function fitMeta(meta, confirming) {
  const [values, energy] = meta.children;
  const [uptime, cpu, memory] = values.children;
  if (!confirming) {
    uptime.classList.remove('meta-overflow');
    cpu.classList.remove('meta-overflow');
    return;
  }
  const gap = parseFloat(getComputedStyle(meta).columnGap);
  const available = meta.getBoundingClientRect().width - energy.getBoundingClientRect().width
    - memory.getBoundingClientRect().width - gap;
  const cpuWidth = cpu.getBoundingClientRect().width + gap;
  const uptimeWidth = uptime.getBoundingClientRect().width + gap;

  // Keep energy and RAM in confirmation, then drop uptime before CPU.
  uptime.classList.toggle('meta-overflow', uptimeWidth + cpuWidth > available);
  cpu.classList.toggle('meta-overflow', cpuWidth > available);
}

function createRow(port) {
  const row = document.createElement('article');
  row.className = 'port-row';
  row.dataset.port = port;
  row.innerHTML = `
    <div class="row-top">
      <span class="port-number"></span>
      <div class="row-main">
        <span class="process-name"></span>
        <div class="row-actions">
          <div class="action-group standard-actions">
            ${button('open', 'Open in browser', icon('open'), 'btn-open')}
            ${button('copy', 'Copy port', icon('copy'), 'btn-copy')}
            ${button('arm', 'Kill process', icon('close'), 'danger')}
            <span class="protected-mark" role="img" title="Protected process" aria-label="Protected process">${icon('lock')}</span>
          </div>
          <div class="action-group confirmation-actions" aria-label="Confirm kill" hidden>
            ${button('kill', 'Kill (SIGTERM)', 'Kill', 'confirm')}
            ${button('force', 'Force kill (SIGKILL)', 'Force', 'force')}
            ${button('cancel', 'Cancel kill', icon('close'))}
          </div>
        </div>
        <button class="icon-button expand-btn" type="button" data-action="expand" aria-controls="details-${port}">${icon('chevron')}</button>
        <div class="meta">
          <div class="meta-values">
            <span class="meta-item" title="Uptime">${icon('uptime')}<span class="uptime-val"></span></span>
            <span class="meta-item" title="CPU">${icon('cpu')}<span class="cpu-val"></span></span>
            <span class="meta-item" title="Memory">${icon('memory')}<span class="mem-val"></span></span>
          </div>
          <span class="meta-item energy" title="Energy impact">${icon('bolt')}<span class="energy-val"></span></span>
        </div>
      </div>
    </div>
    <div class="row-details" id="details-${port}" hidden>
      <dl class="detail-list">
        <dt>PID</dt><dd class="pid-val"></dd>
        <dt>Source</dt><dd class="launch-val"></dd>
        <dt>Command</dt><dd class="command-val"></dd>
        <dt>Directory</dt><dd class="directory-val"></dd>
      </dl>
      <dl class="stats">
        <div><dt>Port</dt><dd class="detail-port"></dd></div>
        <div><dt>Uptime</dt><dd class="uptime-val"></dd></div>
        <div><dt>CPU</dt><dd class="cpu-val"></dd></div>
        <div><dt>Memory</dt><dd class="mem-val"></dd></div>
        <div><dt>Energy</dt><dd class="energy"><span class="energy-val"></span></dd></div>
      </dl>
    </div>`;
  row.querySelectorAll(rowLayoutTargets).forEach((target) => rowLayoutObserver.observe(target));
  return row;
}

function updateRow(row, entry) {
  const energy = entry.cpu_percent < 5 ? 'Low' : entry.cpu_percent < 20 ? 'Medium' : 'High';
  const values = {
    'port-number': `:${entry.port}`,
    'detail-port': `:${entry.port}`,
    'process-name': getDisplayName(entry),
    'launch-val': entry.launched_from || 'service',
    'uptime-val': formatUptime(entry.uptime_seconds),
    'cpu-val': formatCpu(entry.cpu_percent),
    'mem-val': formatMem(entry.mem_mb),
    'energy-val': energy,
    'pid-val': entry.pid || '—',
    'command-val': entry.cmdline || getCommandName(entry.cmdline, entry.process_name),
    'directory-val': getDirectoryContext(entry.cwd, entry.cmdline),
  };
  for (const [name, value] of Object.entries(values)) {
    for (const field of row.getElementsByClassName(name)) {
      if (field.textContent !== String(value)) field.textContent = value;
    }
  }
  for (const field of row.getElementsByClassName('energy')) {
    const impact = energy.toLowerCase();
    if (field.dataset.impact !== impact) field.dataset.impact = impact;
  }

  const protectedEntry = isProtectedEntry(entry);
  const arm = row.querySelector('[data-action="arm"]');
  const busy = killing.has(entry.port);
  setHidden(row.querySelector('.btn-open'), !canOpenInBrowser(entry));
  setHidden(arm, protectedEntry);
  if (arm.disabled !== busy) arm.disabled = busy;
  setHidden(row.querySelector('.protected-mark'), !protectedEntry);
  syncRowState(row);
}

function setHidden(element, hidden) {
  if (element.hidden !== hidden) element.hidden = hidden;
}

function syncRowState(row) {
  const port = Number(row.dataset.port);
  const expanded = port === expandedPort;
  row.classList.toggle('expanded', expanded);
  setHidden(row.querySelector('.row-details'), !expanded);
  const button = row.querySelector('.expand-btn');
  if (button.getAttribute('aria-expanded') !== String(expanded)) {
    const label = expanded ? 'Collapse details' : 'Expand details';
    button.setAttribute('aria-expanded', expanded);
    button.setAttribute('aria-label', label);
    button.dataset.tip = label;
  }
  const confirming = pendingKill?.port === port;
  row.classList.toggle('confirming', confirming);
  setHidden(row.querySelector('.standard-actions'), confirming);
  setHidden(row.querySelector('.confirmation-actions'), !confirming);
}

function findRow(port) {
  return portList.querySelector(`.port-row[data-port="${port}"]`);
}

function setPendingKill(entry, focus = false) {
  const previous = pendingKill;
  pendingKill = entry && { port: entry.port, pids: getKillPids(entry) };
  if (previous) {
    const row = findRow(previous.port);
    if (row) {
      syncRowState(row);
      if (!entry && focus) row.querySelector('[data-action="arm"]').focus({ preventScroll: true });
    }
  }
  if (entry) {
    const row = findRow(entry.port);
    syncRowState(row);
    row.querySelector('[data-action="kill"]').focus({ preventScroll: true });
  }
}

function render() {
  const portsByNumber = new Map(ports.map((entry) => [entry.port, entry]));
  if (pendingKill) {
    const entry = portsByNumber.get(pendingKill.port);
    if (!entry || isProtectedEntry(entry) || getKillPids(entry).join(',') !== pendingKill.pids.join(',')) {
      setPendingKill(null);
    }
  }
  if (!portsByNumber.has(expandedPort)) expandedPort = null;
  const rows = new Map();
  for (const row of [...portList.children]) {
    const port = Number(row.dataset.port);
    if (portsByNumber.has(port)) rows.set(port, row);
    else {
      row.querySelectorAll(rowLayoutTargets).forEach((target) => rowLayoutObserver.unobserve(target));
      row.remove();
    }
  }

  let cursor = portList.firstElementChild;
  for (const entry of ports) {
    const row = rows.get(entry.port) || createRow(entry.port);
    updateRow(row, entry);
    if (row !== cursor) portList.insertBefore(row, cursor);
    cursor = row.nextElementSibling;
  }
  if (ports.length === 0 && emptyState.textContent !== 'No servers running') {
    emptyState.textContent = 'No servers running';
  }
  setHidden(emptyState, ports.length !== 0);
  sizePopup();
}

async function killEntry(entry, pids, force) {
  if (isProtectedEntry(entry) || killing.has(entry.port)) return;
  if (pids.length === 0) {
    showToast('No process ID available to kill.');
    return;
  }
  killing.add(entry.port);
  const arm = findRow(entry.port)?.querySelector('[data-action="arm"]');
  if (arm) arm.disabled = true;
  try {
    await invokeCommand('kill_processes', { port: entry.port, pids, force });
    const row = findRow(entry.port);
    if (row && !matchMedia('(prefers-reduced-motion: reduce)').matches) {
      row.classList.add('leaving');
      await new Promise((resolve) => setTimeout(resolve, 180));
    }
    ports = ports.filter((item) => item.port !== entry.port);
    render();
  } catch (err) {
    showToast(err instanceof Error ? err.message : String(err));
  } finally {
    killing.delete(entry.port);
    if (arm) arm.disabled = false;
    refresh({ supersede: true });
  }
}

function showCopyFeedback(button) {
  const setIcon = (name, label) => {
    button.innerHTML = icon(name);
    button.removeAttribute('title');
    button.dataset.tip = label;
    button.setAttribute('aria-label', label);
    if (tipTarget === button && !tip.hidden) tip.textContent = label;
  };
  clearTimeout(copyTimers.get(button));
  setIcon('check', 'Copied');
  copyTimers.set(button, setTimeout(() => {
    setIcon('copy', 'Copy port');
    copyTimers.delete(button);
  }, 1200));
}

portList.addEventListener('click', async (event) => {
  const top = event.target.closest('.row-top');
  if (!top) return;
  const port = Number(top.closest('.port-row').dataset.port);
  const actionButton = event.target.closest('[data-action]');
  const action = actionButton?.dataset.action || 'expand';
  const entry = ports.find((item) => item.port === port);
  if (!entry) return;

  switch (action) {
    case 'expand': {
      const previous = expandedPort;
      expandedPort = previous === port ? null : port;
      if (previous !== null && previous !== port) syncRowState(findRow(previous));
      syncRowState(findRow(port));
      sizePopup();
      break;
    }
    case 'open':
      if (canOpenInBrowser(entry)) {
        try {
          await invokeCommand('open_in_browser', { url: `http://localhost:${port}` });
        } catch (err) {
          showToast(err instanceof Error ? err.message : String(err));
        }
      }
      break;
    case 'copy':
      try {
        await navigator.clipboard.writeText(String(port));
        showCopyFeedback(actionButton);
      } catch (_) {
        showToast('Failed to copy port.');
      }
      break;
    case 'arm':
      if (!isProtectedEntry(entry)) {
        if (confirmKillToggle.checked) setPendingKill(entry);
        else await killEntry(entry, getKillPids(entry), killActionSelect.value === 'force');
      }
      break;
    case 'kill':
    case 'force':
      if (pendingKill?.port === port) {
        const pids = pendingKill.pids;
        setPendingKill(null);
        await killEntry(entry, pids, action === 'force');
      }
      break;
    case 'cancel':
      setPendingKill(null, true);
      break;
  }
});

document.addEventListener('pointerdown', (event) => {
  if (pendingKill && !event.target.closest('.confirmation-actions')) setPendingKill(null);
});

function setShowAll(enabled) {
  showAllToggle.checked = showAllSettings.checked = enabled;
  localStorage.setItem('harbor-show-all', String(enabled));
}

function applyTheme() {
  const { dataset } = document.documentElement;
  dataset.themeMode = themeSelect.value;
  dataset.systemTheme = systemTheme.matches ? 'dark' : 'light';
}

for (const toggle of [showAllToggle, showAllSettings]) {
  toggle.addEventListener('change', () => {
    setShowAll(toggle.checked);
    refresh({ supersede: true });
  });
}
refreshSelect.addEventListener('change', () => {
  localStorage.setItem('harbor-refresh-interval', refreshSelect.value);
  restartRefreshInterval();
});
confirmKillToggle.addEventListener('change', () => {
  localStorage.setItem('harbor-confirm-kill', String(confirmKillToggle.checked));
});
killActionSelect.addEventListener('change', () => {
  localStorage.setItem('harbor-kill-action', killActionSelect.value);
});
themeSelect.addEventListener('change', () => {
  localStorage.setItem('harbor-theme', themeSelect.value);
  applyTheme();
});
systemTheme.addEventListener('change', applyTheme);
settingsBtn.addEventListener('click', () => showSettings(true));
settingsBack.addEventListener('click', () => showSettings(false));
byId('quit-btn').addEventListener('click', () => {
  invokeCommand('quit_app').catch((err) => showToast(String(err)));
});

function handlePopupFocus(focused) {
  if (popupFocused === focused) return;
  popupFocused = focused;
  restartRefreshInterval();
  if (focused) {
    requestedHeight = 0;
    sizePopup();
    refresh({ supersede: true });
  } else {
    ++latestScanRequest;
    setPendingKill(null);
    // The popup hides on focus loss; the next open always starts on the port list.
    closeDropdowns();
    if (!settingsView.hidden) showSettings(false);
  }
}

window.addEventListener('wheel', (event) => {
  if (event.ctrlKey || event.metaKey) event.preventDefault();
}, { passive: false, capture: true });
for (const gesture of ['gesturestart', 'gesturechange']) {
  window.addEventListener(gesture, (event) => event.preventDefault(), { passive: false });
}
window.addEventListener('dragstart', (event) => event.preventDefault());
document.addEventListener('pointerdown', (event) => {
  // The compositor can route transparent-corner clicks to the WebView.
  if (!event.target.closest('#app')) {
    getCurrentAppWindow().hide().catch((err) => console.error('window hide failed:', err));
  }
});
window.addEventListener('keydown', (event) => {
  if (((event.ctrlKey || event.metaKey) && ['=', '+', '-', '_', '0'].includes(event.key))
    || event.key === 'Backspace' || (event.altKey && event.key === 'ArrowLeft')) {
    event.preventDefault();
  } else if (event.key === 'Escape') {
    if (pendingKill) setPendingKill(null, true);
    else if (!settingsView.hidden) showSettings(false);
    else getCurrentAppWindow().hide().catch((err) => console.error('window hide failed:', err));
  }
});

async function init() {
  themeSelect.value = getSavedTheme();
  refreshSelect.value = String(getSavedRefreshInterval());
  confirmKillToggle.checked = getSavedConfirmBeforeKill();
  killActionSelect.value = getSavedKillAction();
  syncDropdowns();
  setShowAll(getSavedShowAll());
  applyTheme();
  const resizeObserver = new ResizeObserver(sizePopup);
  resizeObserver.observe(portsView);
  resizeObserver.observe(settingsView);
  sizePopup();

  const appWindow = getCurrentAppWindow();
  let focusEventCount = 0;
  await appWindow.onFocusChanged(({ payload }) => {
    ++focusEventCount;
    handlePopupFocus(payload);
  });
  const focusEventCountAtQuery = focusEventCount;
  const focused = await appWindow.isFocused();
  if (focusEventCountAtQuery === focusEventCount) handlePopupFocus(focused);
  // Load the list before the window is shown; markFirstContentReady() then maps it.
  // A recreated popup draws the last scan at once; the scan on focus refreshes it.
  const cached = await invokeCommand('cached_ports', { showAll: showAllToggle.checked }).catch(() => null);
  if (cached && !firstContentReady) {
    ports = cached;
    render();
    markFirstContentReady();
  }
  if (!firstContentReady) refresh();
}


// ─── Tooltips ────────────────────────────────────────────────────────────────
// Native title tooltips are drawn by GTK with a boxed background we can't style,
// so titles are moved into a plain-text tip we position ourselves.
const tip = document.createElement('div');
tip.className = 'tip';
tip.setAttribute('role', 'tooltip');
tip.hidden = true;
document.body.append(tip);
let tipTarget = null;
let tipTimer = 0;

function hideTip() {
  clearTimeout(tipTimer);
  tipTarget = null;
  tip.hidden = true;
}

let pointer = { x: 0, y: 0 };
document.addEventListener('mousemove', (event) => { pointer = { x: event.clientX, y: event.clientY }; });

document.addEventListener('mouseover', (event) => {
  pointer = { x: event.clientX, y: event.clientY };
  const target = event.target.closest('[title], [data-tip]');
  if (target === tipTarget) return;
  hideTip();
  if (!target) return;
  if (target.hasAttribute('title')) {
    target.dataset.tip = target.title;
    target.removeAttribute('title');
  }
  if (!target.dataset.tip) return;
  tipTarget = target;
  tipTimer = setTimeout(() => {
    tip.textContent = target.dataset.tip;
    tip.hidden = false;
    // Sit clear of the cursor graphic (~20 px tall below the hotspot).
    const width = tip.offsetWidth;
    const height = tip.offsetHeight;
    const left = Math.min(Math.max(4, pointer.x - width / 2), window.innerWidth - width - 4);
    const below = pointer.y + 24 + height < window.innerHeight;
    tip.style.left = `${left}px`;
    tip.style.top = `${below ? pointer.y + 24 : pointer.y - height - 10}px`;
  }, 450);
});
document.addEventListener('mousedown', hideTip);
document.addEventListener('scroll', hideTip, true);
window.addEventListener('blur', hideTip);


// ─── GNOME integration ───────────────────────────────────────────────────────
const gnomeSection = byId('gnome-section');
const gnomeStatus = byId('gnome-status');
const gnomeEnable = byId('gnome-enable');

function showGnomeStatus(status) {
  gnomeSection.hidden = !status.available;
  if (!status.available) return;
  let note;
  if (status.active) note = 'Active. harbor opens from its own top-bar button.';
  else if (status.user_extensions_disabled) note = 'Extensions are turned off in GNOME. Turn them on in the Extensions app.';
  else if (status.enabled && status.installed) note = 'Installed. Log out and back in to finish.';
  else note = 'Opens harbor under its own top-bar button with one click.';
  gnomeStatus.textContent = note;
  gnomeEnable.hidden = (status.enabled && status.installed) || status.active;
}

async function refreshGnomeStatus() {
  try {
    showGnomeStatus(await invokeCommand('gnome_integration_status'));
  } catch (err) {
    console.error('gnome_integration_status failed:', err);
  }
}

gnomeEnable.addEventListener('click', async () => {
  gnomeEnable.disabled = true;
  try {
    showGnomeStatus(await invokeCommand('enable_gnome_integration'));
  } catch (err) {
    showToast(err instanceof Error ? err.message : String(err));
  } finally {
    gnomeEnable.disabled = false;
  }
});

settingsBtn.addEventListener('click', refreshGnomeStatus);

// ─── In-page dropdowns ───────────────────────────────────────────────────────
// A native <select> opens its menu as a separate GTK popup window, which takes
// focus from harbor and closes the popup. These menus live inside the page, so
// focus never leaves. The hidden <select> stays the source of truth: it holds the
// value and fires 'change', so the settings code above is unchanged.
const dropdowns = [];

function syncDropdowns() {
  for (const { select, label } of dropdowns) {
    label.textContent = select.selectedOptions[0]?.textContent ?? '';
  }
}

function closeDropdowns(except) {
  for (const dropdown of dropdowns) {
    if (dropdown !== except && !dropdown.list.hidden) {
      dropdown.list.hidden = true;
      dropdown.button.setAttribute('aria-expanded', 'false');
    }
  }
}

for (const select of document.querySelectorAll('.settings-row select')) {
  select.hidden = true;
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'dropdown';
  button.setAttribute('aria-haspopup', 'listbox');
  button.setAttribute('aria-expanded', 'false');
  const label = document.createElement('span');
  button.append(label);
  const list = document.createElement('div');
  list.className = 'dropdown-list';
  list.setAttribute('role', 'listbox');
  list.hidden = true;
  const dropdown = { select, button, label, list };
  for (const option of select.options) {
    const item = document.createElement('div');
    item.className = 'dropdown-option';
    item.setAttribute('role', 'option');
    item.tabIndex = -1;
    item.dataset.value = option.value;
    item.textContent = option.textContent;
    list.append(item);
  }
  const options = () => [...list.children];
  const open = () => {
    closeDropdowns(dropdown);
    for (const item of options()) item.setAttribute('aria-selected', String(item.dataset.value === select.value));
    list.hidden = false;
    // Open upward when the menu would run past the bottom of the popup.
    list.classList.remove('up');
    const viewportBottom = viewport.getBoundingClientRect().bottom;
    list.classList.toggle('up', list.getBoundingClientRect().bottom > viewportBottom);
    button.setAttribute('aria-expanded', 'true');
    (options().find((item) => item.dataset.value === select.value) ?? options()[0]).focus();
  };
  const close = (refocus) => {
    list.hidden = true;
    button.setAttribute('aria-expanded', 'false');
    if (refocus) button.focus();
  };
  const choose = (item) => {
    if (select.value !== item.dataset.value) {
      select.value = item.dataset.value;
      select.dispatchEvent(new Event('change'));
    }
    syncDropdowns();
    close(true);
  };
  button.addEventListener('click', () => (list.hidden ? open() : close(true)));
  button.addEventListener('keydown', (event) => {
    if (['ArrowDown', 'ArrowUp'].includes(event.key)) {
      event.preventDefault();
      open();
    }
  });
  list.addEventListener('click', (event) => {
    const item = event.target.closest('.dropdown-option');
    if (item) choose(item);
  });
  list.addEventListener('keydown', (event) => {
    const items = options();
    const index = items.indexOf(document.activeElement);
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      const step = event.key === 'ArrowDown' ? 1 : -1;
      items[(index + step + items.length) % items.length].focus();
    } else if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      if (index >= 0) choose(items[index]);
    } else if (event.key === 'Escape' || event.key === 'Tab') {
      // Escape closes only the menu, not the whole popup.
      event.preventDefault();
      event.stopPropagation();
      close(true);
    }
  });
  // The row's <label for> pointed at the select; point it at the visible control.
  button.id = `${select.id}-button`;
  select.closest('label')?.setAttribute('for', button.id);
  select.after(button, list);
  dropdowns.push(dropdown);
}
document.addEventListener('mousedown', (event) => {
  if (!event.target.closest('.dropdown, .dropdown-list')) closeDropdowns();
});

// ─── Start at login ──────────────────────────────────────────────────────────
const autostartToggle = byId('autostart-toggle');

async function refreshAutostart() {
  try {
    autostartToggle.checked = await invokeCommand('autostart_enabled');
  } catch (err) {
    console.error('autostart_enabled failed:', err);
  }
}

autostartToggle.addEventListener('change', async () => {
  try {
    autostartToggle.checked = await invokeCommand('set_autostart', { enabled: autostartToggle.checked });
  } catch (err) {
    autostartToggle.checked = !autostartToggle.checked;
    showToast(err instanceof Error ? err.message : String(err));
  }
});

settingsBtn.addEventListener('click', refreshAutostart);

// Start last, after every module-level setup above (tooltips, dropdowns, GNOME) has run.
init().catch((err) => console.error('window initialization failed:', err));
