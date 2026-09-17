/**
 * Bebok Chrome extension - popup logic.
 *
 * Plain ES2019 + async/await (no bundler, no modules). Flow:
 *   1. load settings (`engineUrl`, `directory`) from `chrome.storage.sync`
 *   2. ask the content script for page context (selection + meta + main text)
 *   3. send everything to the background worker (`chrome.runtime.sendMessage`
 *      with `{ type: 'bebok.ask' }`) - the worker owns the engine HTTP calls
 *      because only it has `host_permissions`
 *   4. render the answer + a link to the session in the Bebok UI
 *
 * Message names are shared with `background.js` (see `ext-core`).
 */
'use strict';

/** Message type understood by the background worker. */
const ASK_MESSAGE = 'bebok.ask';
/** Message type understood by the content script. */
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
};

let busy = false;

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

/** Open the options page (gear button / "set up" links). */
function openOptions() {
  if (chrome.runtime && chrome.runtime.openOptionsPage) {
    chrome.runtime.openOptionsPage();
  } else if (chrome.tabs && chrome.tabs.create && chrome.runtime && chrome.runtime.getURL) {
    chrome.tabs.create({ url: chrome.runtime.getURL('options.html') });
  }
}

/** Read `{ engineUrl, directory }` from synced settings.
 * Outside the extension (plain http preview) `chrome.storage` is undefined -
 * resolve empty settings instead of throwing, so the page still renders. */
function readSettings() {
  return new Promise((resolve) => {
    if (!chrome.storage || !chrome.storage.sync) {
      resolve({ engineUrl: '', directory: '' });
      return;
    }
    chrome.storage.sync.get({ engineUrl: '', directory: '' }, resolve);
  });
}

/** Ask the *active tab's* content script for the page snapshot. */
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
          // `chrome.runtime.lastError` is set when no content script answered
          // (chrome:// pages, Web Store, or a tab loaded before install).
          void chrome.runtime.lastError;
          resolve(response && response.ok ? response.context : null);
        }
      );
    });
  });
}

/**
 * Build the prompt sent to the engine: the user's question plus the requested
 * page context blocks. Kept deliberately explicit so the model sees what came
 * from where.
 */
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

/** Point the "Open session" link at the Bebok UI (or the raw API). */
function showSessionLink(sessionID, engineUrl) {
  if (!sessionID || !chrome.tabs) {
    dom.sessionLink.hidden = true;
    return;
  }
  dom.sessionLink.hidden = false;
  dom.sessionLink.href = `${String(engineUrl || '').replace(/\/+$/, '')}/session/${sessionID}/message`;
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

/** Pick up the selection stashed by the context-menu entry (then clear it). */
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

function init() {
  dom.ask.addEventListener('click', ask);
  dom.options.addEventListener('click', openOptions);
  consumePendingSelection();
  dom.question.addEventListener('keydown', (event) => {
    // Ctrl/Cmd+Enter submits like the button; Enter keeps inserting newlines.
    if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') {
      event.preventDefault();
      ask();
    }
  });
  setStatus('');

  readSettings().then((settings) => {
    if (!settings.engineUrl || !settings.directory) {
      setStatus('Configure the engine URL and directory in Options.', 'error');
      dom.target.textContent = '';
    } else {
      dom.target.textContent = `${settings.engineUrl} · ${settings.directory}`;
    }
  });
}

document.addEventListener('DOMContentLoaded', init);
