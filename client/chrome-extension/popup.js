/**
 * Bebok Chrome extension - popup logic.
 */
'use strict';

const ASK_MESSAGE = 'bebok.ask';
const PAGE_CONTEXT_MESSAGE = 'bebok.pageContext';

const el = (id) => document.getElementById(id);

const dom = {
  options: el('options'),
  question: el('question'),
  includeUrl: el('includeUrl'),
  includeSelection: el('includeSelection'),
  includeText: el('includeText'),
  includeScreenshot: el('includeScreenshot'),
  ask: el('ask'),
  status: el('status'),
  answerWrap: el('answerWrap'),
  answer: el('answer'),
  sessionLink: el('sessionLink'),
  target: el('target'),
  statusDot: el('statusDot'),
  copyCmd: el('copyCmd'),
};

let busy = false;
let _engineUrl = '';
let _directory = '';

function setStatus(text, kind) {
  dom.status.textContent = text || '';
  if (kind) {
    dom.status.dataset.kind = kind;
  } else {
    delete dom.status.dataset.kind;
  }
}

function setBusy(value) {
  busy = value;
  dom.ask.disabled = value;
  dom.ask.textContent = value ? 'Asking…' : 'Ask';
}

function openOptions() {
  if (chrome.runtime && chrome.runtime.openOptionsPage) {
    chrome.runtime.openOptionsPage();
  } else if (chrome.tabs && chrome.tabs.create && chrome.runtime && chrome.runtime.getURL) {
    chrome.tabs.create({ url: chrome.runtime.getURL('options.html') });
  }
}

function readSettings() {
  return new Promise((resolve) => {
    if (!chrome.storage || !chrome.storage.sync) {
      resolve({ engineUrl: '', directory: '' });
      return;
    }
    chrome.storage.sync.get({ engineUrl: '', directory: '' }, resolve);
  });
}

function requestPageContext() {
  return new Promise((resolve) => {
    if (!chrome.tabs || !chrome.tabs.query) {
      resolve(null);
      return;
    }
    chrome.tabs.query({ active: true, currentWindow: true }, (tabs) => {
      const tab = tabs && tabs[0];
      if (!tab || !tab.id) {
        resolve(null);
        return;
      }
      chrome.tabs.sendMessage(
        tab.id,
        { type: PAGE_CONTEXT_MESSAGE },
        (response) => {
          void chrome.runtime.lastError;
          resolve(response && response.ok ? response.context : null);
        }
      );
    });
  });
}

function buildPrompt(question, context, options) {
  const blocks = [question.trim()];
  if (context) {
    if (options.includeUrl) {
      blocks.push(
        ['', '---', `Page URL: ${context.url}`, `Page title: ${context.title || '(none)'}`, ''].join(
          '\n'
        )
      );
    }
    if (options.includeSelection && context.selection) {
      blocks.push(['Selected text:', context.selection, ''].join('\n'));
    }
    if (options.includeText && context.text) {
      blocks.push(['Page text:', context.text, ''].join('\n'));
    }
  }
  return blocks.join('\n').trim();
}

function showAnswer(text) {
  dom.answer.textContent = text || '(no answer)';
  dom.answerWrap.hidden = false;
}

function enginePathUrl(engineUrl, path) {
  const trimmed = String(engineUrl || '').trim();
  const url = new URL(trimmed);
  url.pathname = url.pathname.replace(/\/+$/, '') + path;
  return url.toString();
}

function showSessionLink(sessionID, engineUrl) {
  if (!sessionID || !chrome.tabs) {
    dom.sessionLink.hidden = true;
    return;
  }
  dom.sessionLink.hidden = false;
  dom.sessionLink.href = enginePathUrl(engineUrl, `/session/${sessionID}/message`);
  dom.sessionLink.textContent = `Session ${sessionID.slice(0, 8)} ↗`;
}

async function ask() {
  if (busy) return;
  const question = dom.question.value.trim();
  if (!question) {
    setStatus('Type a question first.', 'error');
    dom.question.focus();
    return;
  }

  const settings = await readSettings();
  if (!settings.engineUrl || !settings.directory) {
    setStatus('Set the engine URL and directory in Options first.', 'error');
    dom.target.textContent = '';
    return;
  }

  const options = {
    includeUrl: dom.includeUrl.checked,
    includeSelection: dom.includeSelection.checked,
    includeText: dom.includeText.checked,
    includeScreenshot: dom.includeScreenshot.checked,
  };

  setBusy(true);
  setStatus('Reading page…');
  dom.answerWrap.hidden = true;

  try {
    const context = await requestPageContext();
    if (!context) {
      setStatus(
        'Could not read this page (reload the tab after installing the extension).',
        'error'
      );
      setBusy(false);
      return;
    }

    setStatus('Asking Bebok…');
    const result = await new Promise((resolve) => {
      chrome.runtime.sendMessage(
        {
          type: ASK_MESSAGE,
          question: buildPrompt(question, context, options),
          directory: settings.directory,
          engineUrl: settings.engineUrl,
          screenshot: options.includeScreenshot,
        },
        (response) => {
          void chrome.runtime.lastError;
          resolve(response);
        }
      );
    });

    if (!result || !result.ok) {
      const error = (result && result.error) || 'unknown error';
      setStatus(`Failed: ${error}`, 'error');
      return;
    }
    showAnswer(result.answer);
    showSessionLink(result.sessionID, settings.engineUrl);
    setStatus(result.answer ? 'Done.' : 'Session started, no text answer yet.');
  } finally {
    setBusy(false);
    dom.target.textContent = `${settings.engineUrl} · ${settings.directory}`;
  }
}

function consumePendingSelection() {
  if (!chrome.storage || !chrome.storage.session) return;
  chrome.storage.session.get({ pendingSelection: null }, (values) => {
    const pending = values && values.pendingSelection;
    if (!pending || !pending.text) return;
    chrome.storage.session.remove('pendingSelection', () => {});
    const current = dom.question.value.trim();
    const quote = pending.text.trim();
    const prefix = pending.url ? `(from ${pending.url})\n` : '';
    dom.question.value = current
      ? `${current}\n\n${prefix}> ${quote}`
      : `${prefix}> ${quote}\n\n`;
    dom.question.focus();
  });
}

// ── Status probe (Phase 1) ────────────────────────────────────────────────

async function probeVersion() {
  if (!_engineUrl) {
    setStatusDot('');
    return;
  }
  try {
    const url = enginePathUrl(_engineUrl, '/version');
    const res = await fetch(url, { signal: AbortSignal.timeout(2000) });
    if (!res.ok) throw new Error('HTTP ' + res.status);
    const data = await res.json();
    setStatusDot('●', 'ok');
  } catch (_) {
    setStatusDot('●', 'error');
  }
}

function setStatusDot(char, kind) {
  if (dom.statusDot) {
    dom.statusDot.textContent = char || '';
    if (kind) {
      dom.statusDot.dataset.kind = kind;
    } else {
      delete dom.statusDot.dataset.kind;
    }
  }
}

function copyStartCommand() {
  const token = localStorage.getItem('bebokToken') || '';
  const port = localStorage.getItem('fixedPort') || '8787';
  const cmd = `BEBOK_TOKEN=${token} bebok-server --port ${port}`;
  navigator.clipboard.writeText(cmd).then(() => {
    setStatus('Command copied to clipboard.', 'ok');
    setTimeout(() => setStatus(''), 2000);
  }, () => {
    setStatus('Failed to copy command.', 'error');
  });
}

function init() {
  dom.ask.addEventListener('click', ask);
  dom.options.addEventListener('click', openOptions);
  dom.copyCmd.addEventListener('click', copyStartCommand);
  consumePendingSelection();
  dom.question.addEventListener('keydown', (event) => {
    if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') {
      event.preventDefault();
      ask();
    }
  });
  setStatus('');

  readSettings().then((settings) => {
    _engineUrl = settings.engineUrl;
    _directory = settings.directory;
    if (!_engineUrl || !_directory) {
      setStatus('Configure the engine URL and directory in Options.', 'error');
      dom.target.textContent = '';
      setStatusDot('');
    } else {
      dom.target.textContent = `${_engineUrl} · ${_directory}`;
      setStatusDot('●', 'pending');
      // Probe GET /version every 10s
      probeVersion();
      setInterval(probeVersion, 10000);
    }
  });
}

document.addEventListener('DOMContentLoaded', init);