/**
 * App-wide undo / redo for `<textarea>` and text `<input>` fields.
 *
 * The webview's native undo stack is unreliable (WebKitGTK in the Tauri
 * shell drops it, programmatic value writes from Angular reset it) and
 * Ctrl+Y is not a redo shortcut on Linux/macOS at all. This keeps our own
 * history per element and handles Ctrl/Cmd+Z, Ctrl/Cmd+Y and
 * Ctrl/Cmd+Shift+Z.
 *
 * Typing is grouped into word-sized steps (a step closes on whitespace, a
 * change of input kind or a pause). A value written by code (e.g. the
 * explorer loading another file) starts a fresh history, so undo never
 * resurrects an unrelated document. The xterm helper textarea and
 * contenteditable editors keep their own key handling.
 */

interface Snap {
  value: string;
  start: number;
  end: number;
}

interface History {
  undo: Snap[];
  redo: Snap[];
  /** The value as of the last recorded change. */
  last: Snap;
  lastKind: string;
  lastAt: number;
}

type Field = HTMLTextAreaElement | HTMLInputElement;

const TEXT_INPUT_TYPES = new Set(['text', 'search', 'url', 'email', 'tel', '']);
const GROUP_MS = 800;
const MAX_STEPS = 500;

function asField(target: EventTarget | null): Field | null {
  const el = target as HTMLElement | null;
  if (!el || typeof el.closest !== 'function') {
    return null;
  }
  if (el.closest('.xterm, [data-native-undo]')) {
    return null;
  }
  if (el instanceof HTMLTextAreaElement) {
    return el.readOnly ? null : el;
  }
  if (el instanceof HTMLInputElement && TEXT_INPUT_TYPES.has(el.type)) {
    return el.readOnly ? null : el;
  }
  return null;
}

function snap(el: Field): Snap {
  return { value: el.value, start: el.selectionStart ?? 0, end: el.selectionEnd ?? 0 };
}

/** Install the document-level listeners; returns an uninstaller. */
export function installTextUndo(doc: Document = document): () => void {
  const histories = new WeakMap<Field, History>();
  let restoring = false;

  const fresh = (el: Field): History => {
    const h: History = { undo: [], redo: [], last: snap(el), lastKind: '', lastAt: 0 };
    histories.set(el, h);
    return h;
  };

  /** History for `el`, reset when code changed the value behind our back. */
  const historyOf = (el: Field): History => {
    const h = histories.get(el);
    return h && h.last.value === el.value ? h : fresh(el);
  };

  const onFocusIn = (event: Event): void => {
    const el = asField(event.target);
    if (el) {
      historyOf(el);
    }
  };

  // Capture the caret before the edit so an undo puts it back there.
  const onBeforeInput = (event: Event): void => {
    const el = asField(event.target);
    if (el && !restoring) {
      const h = historyOf(el);
      h.last = { ...h.last, start: el.selectionStart ?? 0, end: el.selectionEnd ?? 0 };
    }
  };

  const onInput = (event: Event): void => {
    const el = asField(event.target);
    if (!el || restoring) {
      return;
    }
    const h = histories.get(el);
    if (!h) {
      fresh(el);
      return;
    }
    if (h.last.value === el.value) {
      return;
    }
    const ie = event as InputEvent;
    const kind = ie.inputType ?? 'insert';
    const now = Date.now();
    const data = ie.data ?? '';
    const typing = kind === 'insertText' || kind.startsWith('delete');
    const grouped =
      typing && kind === h.lastKind && now - h.lastAt < GROUP_MS && !/\s/.test(data) && h.undo.length > 0;
    if (!grouped) {
      h.undo.push(h.last);
      if (h.undo.length > MAX_STEPS) {
        h.undo.shift();
      }
    }
    h.redo = [];
    h.last = snap(el);
    h.lastKind = /\s/.test(data) ? '' : kind;
    h.lastAt = now;
  };

  const apply = (el: Field, s: Snap): void => {
    restoring = true;
    try {
      el.value = s.value;
      el.setSelectionRange(s.start, s.end);
      // Let ngModel / (input) bindings see the restored value.
      el.dispatchEvent(new Event('input', { bubbles: true }));
    } finally {
      restoring = false;
    }
  };

  const onKeyDown = (event: KeyboardEvent): void => {
    const mod = event.ctrlKey || event.metaKey;
    if (!mod || event.altKey || event.defaultPrevented) {
      return;
    }
    const key = event.key.toLowerCase();
    const isUndo = key === 'z' && !event.shiftKey;
    const isRedo = (key === 'z' && event.shiftKey) || (key === 'y' && !event.shiftKey);
    if (!isUndo && !isRedo) {
      return;
    }
    const el = asField(event.target);
    if (!el) {
      return;
    }
    event.preventDefault();
    const h = historyOf(el);
    const from = isUndo ? h.undo : h.redo;
    const to = isUndo ? h.redo : h.undo;
    const target = from.pop();
    if (!target) {
      return;
    }
    to.push(snap(el));
    apply(el, target);
    h.last = target;
    h.lastKind = '';
  };

  doc.addEventListener('focusin', onFocusIn, true);
  doc.addEventListener('beforeinput', onBeforeInput, true);
  doc.addEventListener('input', onInput, true);
  doc.addEventListener('keydown', onKeyDown, true);
  return () => {
    doc.removeEventListener('focusin', onFocusIn, true);
    doc.removeEventListener('beforeinput', onBeforeInput, true);
    doc.removeEventListener('input', onInput, true);
    doc.removeEventListener('keydown', onKeyDown, true);
  };
}
