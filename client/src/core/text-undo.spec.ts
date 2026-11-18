import { installTextUndo } from './text-undo';

describe('installTextUndo', () => {
  let uninstall: () => void;
  let area: HTMLTextAreaElement;

  const type = (text: string, inputType = 'insertText'): void => {
    area.dispatchEvent(new InputEvent('beforeinput', { bubbles: true, inputType, data: text }));
    area.value += text;
    area.setSelectionRange(area.value.length, area.value.length);
    area.dispatchEvent(new InputEvent('input', { bubbles: true, inputType, data: text }));
  };
  const key = (k: string, shift = false): KeyboardEvent => {
    const ev = new KeyboardEvent('keydown', { key: k, ctrlKey: true, shiftKey: shift, bubbles: true, cancelable: true });
    area.dispatchEvent(ev);
    return ev;
  };

  beforeEach(() => {
    uninstall = installTextUndo(document);
    area = document.createElement('textarea');
    document.body.appendChild(area);
    area.focus();
  });

  afterEach(() => {
    uninstall();
    area.remove();
  });

  it('undoes word by word and redoes with Ctrl+Y and Ctrl+Shift+Z', () => {
    for (const ch of 'foo bar') {
      type(ch);
    }
    expect(area.value).toBe('foo bar');
    expect(key('z').defaultPrevented).toBeTrue();
    expect(area.value).toBe('foo ');
    key('z');
    expect(area.value).toBe('foo');
    key('z');
    expect(area.value).toBe('');
    key('y');
    expect(area.value).toBe('foo');
    key('z', true);
    expect(area.value).toBe('foo ');
  });

  it('notifies bindings through an input event on undo', () => {
    type('x');
    const seen: string[] = [];
    area.addEventListener('input', () => seen.push(area.value));
    key('z');
    expect(seen).toEqual(['']);
  });

  it('starts a fresh history when code replaces the value', () => {
    type('a');
    area.value = 'other file';
    key('z');
    expect(area.value).toBe('other file');
  });

  it('leaves the xterm helper textarea alone', () => {
    area.classList.add('xterm');
    type('a');
    expect(key('z').defaultPrevented).toBeFalse();
  });
});
