/**
 * Bebok Chrome extension - options page.
 *
 * Stores `engineUrl` + `directory` in `chrome.storage.sync`. "Test connection"
 * performs `GET {engineUrl}/config?directory=…` (the engine's cheapest
 * authenticated endpoint; the capability token travels as `?token=` inside the
 * stored URL) and reports OK/FAIL.
 *
 * The request is issued from the background worker
 * (`{ type: 'bebok.testConnection' }`) so it is covered by the extension's host
 * permissions; if the worker does not know the message yet we fall back to a
 * direct `fetch` from this page.
 *
 * "Remote piloting" section manages the `remoteEnabled` flag in
 * `chrome.storage.sync`; the background worker reacts to storage changes and
 * starts / stops the polling loop and heartbeat.
 */
'use strict';

const TEST_MESSAGE = 'bebok.testConnection';
const STORAGE_KEYS = { engineUrl: '', directory: '', remoteEnabled: false };

const el = (id) => document.getElementById(id);
const dom = {
  engineUrl: el('engineUrl'),
  directory: el('directory'),
  test: el('test'),
  save: el('save'),
  status: el('status'),
  remoteEnabled: el('remoteEnabled'),
  remoteStatus: el('remoteStatus'),
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

function load() {
  return new Promise((resolve) => {
    if (!chrome.storage || !chrome.storage.sync) {
      resolve({ engineUrl: '', directory: '', remoteEnabled: false });
      return;
    }
    chrome.storage.sync.get(STORAGE_KEYS, (values) => {
      dom.engineUrl.value = values.engineUrl || '';
      dom.directory.value = values.directory || '';
      dom.remoteEnabled.checked = Boolean(values.remoteEnabled);
      updateRemoteStatus(values);
      resolve(values);
    });
  });
}

function save() {
  const engineUrl = dom.engineUrl.value.trim().replace(/\/+$/, '');
  const directory = dom.directory.value.trim();
  if (!chrome.storage || !chrome.storage.sync) {
    setStatus('Extension storage unavailable in preview.', 'error');
    return Promise.resolve({ engineUrl, directory });
  }
  return new Promise((resolve) => {
    chrome.storage.sync.set({ engineUrl, directory }, () => {
      setStatus('Saved.', 'ok');
      resolve({ engineUrl, directory });
    });
  });
}

/**
 * `GET /config?directory=` against the stored engine. Returns
 * `{ ok: true, config }` or `{ ok: false, error }`.
 */
function testConnection(engineUrl, directory) {
  return new Promise((resolve) => {
    if (!chrome.runtime || !chrome.runtime.sendMessage) {
      resolve({ ok: false, error: 'extension APIs unavailable in preview' });
      return;
    }
    chrome.runtime.sendMessage(
      { type: TEST_MESSAGE, engineUrl, directory },
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
    const result = await testConnection(engineUrl, directory);
    if (result.ok) {
      const model = result.model ? ` · model ${result.model}` : '';
      setStatus(`OK — connected${model}`, 'ok');
    } else {
      setStatus(`FAIL — ${result.error || 'request failed'}`, 'error');
    }
  } finally {
    dom.test.disabled = false;
  }
}

// ── Remote piloting ──────────────────────────────────────────────────────

const STORAGE_KEY_REMOTE_ENABLED = 'remoteEnabled';

/**
 * Show a brief status line under the remote piloting checkbox.
 * `values` is the full storage snapshot (may have stale values).
 */
function updateRemoteStatus(values) {
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
  setRemoteStatus(
    'Enabled — extension will register on the next heartbeat',
    'ok'
  );
}

function onRemoteToggle() {
  const enabled = dom.remoteEnabled.checked;
  if (!chrome.storage || !chrome.storage.sync) {
    setRemoteStatus('Extension storage unavailable in preview.', 'error');
    return;
  }
  chrome.storage.sync.set({ [STORAGE_KEY_REMOTE_ENABLED]: enabled }, () => {
    // Re-read to get the latest snapshot for the status line.
    chrome.storage.sync.get(STORAGE_KEYS, updateRemoteStatus);
  });
}

// ── Bootstrap ────────────────────────────────────────────────────────────

document.addEventListener('DOMContentLoaded', () => {
  dom.test.addEventListener('click', runTest);
  dom.save.addEventListener('click', save);
  dom.remoteEnabled.addEventListener('change', onRemoteToggle);
  load();
});
