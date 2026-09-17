/**
 * Bebok Chrome extension - content script.
 *
 * Injected into the page (see `manifest.json` `content_scripts`, or on demand
 * via `chrome.scripting.executeScript` from the popup). It only *reads* the
 * page when explicitly asked: the popup (or the background worker) sends
 * `{ type: 'bebok.pageContext' }` and gets back a compact snapshot:
 *
 *   { url, title, selection, metaDescription, text }
 *
 * `text` is the main readable content, collapsed and capped (~4000 chars by
 * default) so a prompt stays inside a sane context budget. Nothing is sent
 * anywhere from here - the reply travels to the asker only.
 */
(() => {
  'use strict';

  /** Default cap for the extracted page text (characters). */
  const DEFAULT_MAX_CHARS = 4000;

  /** Nodes that never carry readable content. */
  const NOISE_SELECTOR =
    'script, style, noscript, svg, canvas, template, nav, header, footer, aside, form, iframe, [aria-hidden="true"], [hidden]';

  /** Preferred content containers, in order - the first non-empty one wins. */
  const CONTENT_SELECTORS = [
    'article',
    'main',
    '[role="main"]',
    '#content',
    '#main',
    '#main-content',
    '.post-content',
    '.markdown-body',
    '.content',
  ];

  /**
   * Collapse runs of whitespace/newlines into single spaces so the extracted
   * text does not blow up the prompt with indentation and blank lines.
   */
  function normalize(value) {
    return String(value || '')
      .replace(/\s+/g, ' ')
      .trim();
  }

  /** `<meta name="description">` (fallback: OG description / twitter). */
  function metaDescription() {
    const pick = (selector) =>
      document.querySelector(selector)?.getAttribute('content') || '';
    return normalize(
      pick('meta[name="description"]') ||
        pick('meta[property="og:description"]') ||
        pick('meta[name="twitter:description"]')
    );
  }

  /** Text currently selected in the page (empty when nothing is selected). */
  function selection() {
    return normalize(window.getSelection()?.toString());
  }

  /**
   * Best-effort main text: the first preferred container that yields enough
   * text, otherwise the whole body with the noise nodes removed.
   */
  function mainText() {
    const clean = (root) => {
      if (!root) return '';
      const clone = root.cloneNode(true);
      clone.querySelectorAll(NOISE_SELECTOR).forEach((node) => node.remove());
      return normalize(clone.textContent || '');
    };

    for (const selector of CONTENT_SELECTORS) {
      const text = clean(document.querySelector(selector));
      // A container that only holds a nav strip is not "main content".
      if (text.length >= 200) return text;
    }
    return clean(document.body);
  }

  /** Build the page snapshot sent back to popup/background. */
  function collectPageContext(maxChars) {
    const cap = Number(maxChars) > 0 ? Number(maxChars) : DEFAULT_MAX_CHARS;
    const text = mainText();
    return {
      url: location.href,
      title: document.title || '',
      selection: selection(),
      metaDescription: metaDescription(),
      text: text.length > cap ? `${text.slice(0, cap)}…` : text,
      truncated: text.length > cap,
    };
  }

  chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
    if (!message || message.type !== 'bebok.pageContext') return undefined;
    try {
      sendResponse({ ok: true, context: collectPageContext(message.maxChars) });
    } catch (error) {
      sendResponse({ ok: false, error: String(error?.message || error) });
    }
    return false; // synchronous reply
  });
})();
