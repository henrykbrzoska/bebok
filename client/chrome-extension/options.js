/**
 * Bebok Chrome extension - options page.
 */
'use strict';

const TEST_MESSAGE = 'bebok.testConnection';
const SEARCH_MESSAGE = 'bebok.searchLocal';
const REMOTE_STATUS_MESSAGE = 'bebok.remoteStatus';
const DISCOVERY_STATUS_MESSAGE = 'bebok.discoveryStatus';

const STORAGE_KEYS = { 
  engineUrl: '', 
  directory: '', 
  pinnedToken: '', 
  remoteEnabled: false 
};
const REMOTE_STATUS_INTERVAL_MS = 10_000;

let _remoteStatusTimer = null;

const el = (id) => document.getElementById(id);
const dom = {
  engineUrl: el('engineUrl'),
  directory: el('directory'),
  pinnedToken: el('pinnedToken'),
  test: el('test'),
  search: el('search'),
  save: el('save'),
  status: el('status'),
  remoteEnabled: el('remoteEnabled'),
  remoteStatus: el('remoteStatus'),
  // Phase 1: discovery
  discoveryStatus: el('discoveryStatus'),
  rescan: el('rescan'),
  lastPort: el('lastPort'),
  // Phase 1: server control
  bebokToken: el('bebokToken'),
  genToken: el('genToken'),
  fixedPort: el('fixedPort'),
  startCmd: el('startCmd'),
  copyStartCmd: el('copyStartCmd'),
};

function setStatus(text, kind) {
  dom.status.textContent = text || '';
  if (kind) {
    dom.status.dataset.kind = kind;
  } else {
    delete dom.status.dataset.kind;
  }
}

function setRemoteStatus(text, kind) {
  dom.remoteStatus.textContent = text || '';
  if (kind) {
    dom.remoteStatus.dataset.kind = kind;
  } else {
    delete dom.remoteStatus.dataset.kind;
  }
}

function setDiscoveryStatus(text, kind) {
  dom.discoveryStatus.textContent = text || '';
  if (kind) {
    dom.discoveryStatus.dataset.kind = kind;
  } else {
    delete dom.discoveryStatus.dataset.kind;
  }
}

function load() {
  return new Promise((resolve) => {
    if (!chrome.storage || !chrome.storage.sync) {
      resolve({ engineUrl: '', directory: '', remoteEnabled: false });
      return;
    }
    chrome.storage.sync.get(STORAGE_KEYS, (values) => {
      dom.engineUrl.value = values.engineUrl || '';
      dom.directory.value = values.directory || '';
      dom.pinnedToken.value = values.pinnedToken || '';
      dom.remoteEnabled.checked = Boolean(values.remoteEnabled);
      updateRemoteStatusFallback(values);
      refreshRemoteStatus();
      resolve(values);
    });
  });
}

function save() {
  const engineUrl = dom.engineUrl.value.trim().replace(/\/+$/, '');
  const directory = dom.directory.value.trim();
  const pinnedToken = dom.pinnedToken.value.trim();
  if (!chrome.storage || !chrome.storage.sync) {
    setStatus('Extension storage unavailable in preview.', 'error');
    return Promise.resolve({ engineUrl, directory, pinnedToken });
  }
  return new Promise((resolve) => {
    chrome.storage.sync.set({ engineUrl, directory, pinnedToken }, () => {
      setStatus('Saved.', 'ok');
      resolve({ engineUrl, directory, pinnedToken });
    });
  });
}

function saveSilently(engineUrl, directory, pinnedToken) {
  return new Promise((resolve) => {
    if (!chrome.storage || !chrome.storage.sync) {
      resolve(false);
      return;
    }
    const patch = { engineUrl };
    if (directory !== undefined) patch.directory = directory;
    if (pinnedToken !== undefined) patch.pinnedToken = pinnedToken;
    chrome.storage.sync.set(patch, () => resolve(true));
  });
}

function fillForm(engineUrl, directory) {
  if (engineUrl !== undefined) dom.engineUrl.value = engineUrl;
  if (directory !== undefined) dom.directory.value = directory;
}

function testConnection(engineUrl, directory, pinnedToken) {
  return new Promise((resolve) => {
    if (!chrome.runtime || !chrome.runtime.sendMessage) {
      resolve({ ok: false, error: 'extension APIs unavailable in preview' });
      return;
    }
    chrome.runtime.sendMessage(
      { type: TEST_MESSAGE, engineUrl, directory, pinnedToken },
      (response) => {
        if (chrome.runtime.lastError || !response) {
          resolve({
            ok: false,
            error:
              (chrome.runtime.lastError && chrome.runtime.lastError.message) ||
              'background worker did not answer - reload the extension',
          });
          return;
        }
        resolve(response);
      }
    );
  });
}

async function runTest() {
  const engineUrl = dom.engineUrl.value.trim().replace(/\/+$/, '');
  const directory = dom.directory.value.trim();
  const pinnedToken = dom.pinnedToken.value.trim();
  if (!engineUrl) {
    setStatus('Enter the engine URL first.', 'error');
    return;
  }
  if (!directory) {
    setStatus('Enter the working directory first.', 'error');
    return;
  }
  dom.test.disabled = true;
  setStatus('Testing…');
  try {
    const result = await testConnection(engineUrl, directory, pinnedToken);
    if (result.ok) {
      const model = result.model ? ` · model ${result.model}` : '';
      await saveSilently(engineUrl, directory, pinnedToken || undefined);
      fillForm(engineUrl, directory);
      setStatus(`OK — connected${model} · saved`, 'ok');
      refreshRemoteStatus();
    } else {
      setStatus(`FAIL — ${result.error || 'request failed'}`, 'error');
    }
  } finally {
    dom.test.disabled = false;
  }
}

function searchLocal(engineUrl, directory, pinnedToken) {
  return new Promise((resolve) => {
    if (!chrome.runtime || !chrome.runtime.sendMessage) {
      resolve({ ok: false, error: 'extension APIs unavailable in preview' });
      return;
    }
    chrome.runtime.sendMessage(
      { type: SEARCH_MESSAGE, engineUrl, directory, pinnedToken },
      (response) => {
        if (chrome.runtime.lastError || !response) {
          resolve({
            ok: false,
            error:
              (chrome.runtime.lastError && chrome.runtime.lastError.message) ||
              'background worker did not answer - reload the extension',
          });
          return;
        }
        resolve(response);
      }
    );
  });
}

async function runSearch() {
  const engineUrl = dom.engineUrl.value.trim().replace(/\/+$/, '');
  const directory = dom.directory.value.trim();
  const pinnedToken = dom.pinnedToken.value.trim();
  dom.search.disabled = true;
  setStatus('Searching…', '');
  try {
    const result = await searchLocal(engineUrl, directory, pinnedToken);
    if (result.ok && result.engineUrl) {
      await saveSilently(result.engineUrl, directory || undefined, pinnedToken || undefined);
      fillForm(result.engineUrl, directory || undefined);
      setStatus(`Found local Bebok at ${result.engineUrl} · saved`, 'ok');
      refreshRemoteStatus();
    } else if (result.needsToken) {
      setStatus(`${result.error || 'Engine found, token needed.'}`, 'error');
    } else {
      setStatus(`Not found — ${result.error || 'request failed'}`, 'error');
    }
  } finally {
    dom.search.disabled = false;
  }
}

// ── Remote piloting ──────────────────────────────────────────────────────

const STORAGE_KEY_REMOTE_ENABLED = 'remoteEnabled';

function updateRemoteStatusFallback(values) {
  const enabled = Boolean(values && values.remoteEnabled);
  const hasUrl = Boolean(values && values.engineUrl);
  const hasDir = Boolean(values && values.directory);

  if (!enabled) {
    setRemoteStatus('Disabled', '');
    return;
  }
  if (!hasUrl || !hasDir) {
    setRemoteStatus(
      'Enabled but engine URL or directory is not set',
      'error'
    );
    return;
  }
  setRemoteStatus('Checking…', '');
}

function onRemoteToggle() {
  const enabled = dom.remoteEnabled.checked;
  if (!chrome.storage || !chrome.storage.sync) {
    setRemoteStatus('Extension storage unavailable in preview.', 'error');
    return;
  }
  chrome.storage.sync.set({ [STORAGE_KEY_REMOTE_ENABLED]: enabled }, () => {
    refreshRemoteStatus();
  });
}

function queryRemoteStatus() {
  const engineUrl = dom.engineUrl.value.trim().replace(/\/+$/, '');
  const directory = dom.directory.value.trim();
  return new Promise((resolve) => {
    if (!chrome.runtime || !chrome.runtime.sendMessage) {
      resolve({ ok: false, error: 'extension APIs unavailable in preview' });
      return;
    }
    chrome.runtime.sendMessage(
      { type: REMOTE_STATUS_MESSAGE, engineUrl, directory },
      (response) => {
        if (chrome.runtime.lastError || !response) {
          resolve({
            ok: false,
            error:
              (chrome.runtime.lastError && chrome.runtime.lastError.message) ||
              'background worker did not answer - reload the extension',
          });
          return;
        }
        resolve(response);
      }
    );
  });
}

async function refreshRemoteStatus() {
  if (!chrome.storage || !chrome.storage.sync) {
    setRemoteStatus('Extension storage unavailable in preview.', 'error');
    return;
  }
  chrome.storage.sync.get(STORAGE_KEYS, async (values) => {
    const enabled = Boolean(values && values.remoteEnabled);
    armRemoteStatusTimer(enabled);
    if (!enabled) {
      setRemoteStatus('Disabled', '');
      return;
    }
    if (!values.engineUrl || !values.directory) {
      setRemoteStatus(
        'Enabled but engine URL or directory is not set',
        'error'
      );
      return;
    }
    setRemoteStatus('Checking…', '');
    const result = await queryRemoteStatus();
    if (!result.ok) {
      const e = String(result.error || '');
      const hint = e.includes('401')
        ? 'Token rejected? Paste the new BEBOK_READY URL (token rotated?), Save, Test connection.'
        : e.includes('Failed to fetch')
          ? 'Cannot reach engine — is it running? Check the port.'
          : 'Wake the extension (keep this page open), reload it, or untick+retick remote piloting.';
      setRemoteStatus(
        `Not registered — ${result.error || 'request failed'}. ${hint}`,
        'error'
      );
      return;
    }
    const status = result.status || {};
    const entries = (status.extensions || []).filter(
      (e) => !e.session_id || e.session_id === 'extension'
    );
    const entry = entries[0];
    const queue = status.queue || {};
    const queued = Object.values(queue).reduce(
      (sum, depth) => sum + (Number(depth) || 0),
      0
    );
    if (!entry) {
      setRemoteStatus(
        'Not registered — wake the extension (keep this page open), reload it, or untick+retick remote piloting.',
        'error'
      );
      return;
    }
    const age = entry.last_seen_age_secs;
    const ageText =
      typeof age === 'number' ? `last beat ${age}s ago` : 'heartbeat seen';
    const staleNote = (typeof age === 'number' && age > 30)
      ? ' (worker may be asleep — open this Options page to wake it)'
      : '';
    setRemoteStatus(
      `Registered · ${ageText}${staleNote} · queued commands: ${queued}`,
      'ok'
    );
  });
}

function armRemoteStatusTimer(enabled) {
  if (_remoteStatusTimer) {
    clearInterval(_remoteStatusTimer);
    _remoteStatusTimer = null;
  }
  if (enabled && typeof setInterval !== 'undefined') {
    _remoteStatusTimer = setInterval(refreshRemoteStatus, REMOTE_STATUS_INTERVAL_MS);
  }
}

function updateRemoteStatus() {
  refreshRemoteStatus();
}

// ── Engine discovery (Phase 1) ────────────────────────────────────────────

async function queryDiscoveryStatus() {
  const directory = dom.directory.value.trim();
  return new Promise((resolve) => {
    if (!chrome.runtime || !chrome.runtime.sendMessage) {
      resolve({ ok: false, error: 'extension APIs unavailable in preview' });
      return;
    }
    chrome.runtime.sendMessage(
      { type: DISCOVERY_STATUS_MESSAGE, directory },
      (response) => {
        if (chrome.runtime.lastError || !response) {
          resolve({
            ok: false,
            error:
              (chrome.runtime.lastError && chrome.runtime.lastError.message) ||
              'background worker did not answer - reload the extension',
          });
          return;
        }
        resolve(response);
      }
    );
  });
}

async function runRescan() {
  dom.rescan.disabled = true;
  setDiscoveryStatus('Scanning…', '');
  try {
    const result = await queryDiscoveryStatus();
    if (result.ok && result.port) {
      dom.lastPort.textContent = result.port;
      setDiscoveryStatus(`Found on port ${result.port} (v${result.version || 'unknown'})`, 'ok');
    } else {
      setDiscoveryStatus(`Not found — ${result.error || 'request failed'}`, 'error');
    }
  } finally {
    dom.rescan.disabled = false;
  }
}

// ── Server control (Phase 1) ──────────────────────────────────────────────

function generateToken() {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let token = '';
  for (let i = 0; i < 64; i++) {
    token += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  dom.bebokToken.value = token;
  localStorage.setItem('bebokToken', token);
  updateStartCmd();
}

function getFixedPort() {
  const port = dom.fixedPort.value || '8787';
  localStorage.setItem('fixedPort', port);
  localStorage.setItem('useFixedPort', 'true');
  updateStartCmd();
  return port;
}

function updateStartCmd() {
  const token = localStorage.getItem('bebokToken') || '';
  const port = localStorage.getItem('fixedPort') || '8787';
  dom.startCmd.value = `BEBOK_TOKEN=${token} bebok-server --port ${port}`;
}

function copyStartCommand() {
  const cmd = dom.startCmd.value;
  navigator.clipboard.writeText(cmd).then(() => {
    setStatus('Command copied to clipboard.', 'ok');
    setTimeout(() => setStatus(''), 2000);
  }, () => {
    setStatus('Failed to copy command.', 'error');
  });
}

// ── Bootstrap ────────────────────────────────────────────────────────────

document.addEventListener('DOMContentLoaded', () => {
  dom.test.addEventListener('click', runTest);
  dom.search.addEventListener('click', runSearch);
  dom.save.addEventListener('click', save);
  dom.remoteEnabled.addEventListener('change', onRemoteToggle);
  
  // Phase 1: discovery
  dom.rescan.addEventListener('click', runRescan);
  
  // Phase 1: server control
  dom.genToken.addEventListener('click', generateToken);
  dom.fixedPort.addEventListener('change', getFixedPort);
  dom.copyStartCmd.addEventListener('click', copyStartCommand);
  
  // Initialize start command
  updateStartCmd();
  
  load();
});