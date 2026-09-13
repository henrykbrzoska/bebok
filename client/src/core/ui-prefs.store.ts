/**
 * Chrome preferences: persisted layout state for the app shell.
 *
 * Everything here is client-side only (localStorage) - the engine is never
 * involved. Two generations of keys live side by side:
 *
 * - `sidebarVisible` / `sidebarWidth` belong to the chat session sidebar
 *   (`ui/session-sidebar`), untouched by the redesign.
 * - `sidebarExpanded`, `rightDrawerOpen`, `rightDrawerPanels` and
 *   `rightDrawerWidth` are the WP-SHELL redesign state (F1-3 / F1-12).
 *   The `density` toggle that used to live alongside them (itself a
 *   replacement for the older `topbarCompact` flag) was removed in F7-2:
 *   compact spacing is now the app's only layout (see `styles.css`), so
 *   there is nothing left to persist or choose.
 * - `expandToolCallsByDefault` is the chat transcript preference (F6-1):
 *   when on, every tool call (and every grouped run of tool calls) starts
 *   expanded; when off, only the first tool call of a turn does.
 * - `rightDrawerCollapsed` (F9-2): per-panel collapsed/expanded state of the
 *   stacked drawer sections (a collapsed panel keeps its pill on and its
 *   sticky header visible, only the body is hidden). `rightDrawerReveal` is
 *   the ephemeral "open this panel and scroll it into view" request that
 *   programmatic opens (Preview from a chat link, Browser when a browser tool
 *   fires, Agents when a sub-agent spawns) raise - never persisted.
 */

import { Injectable, signal } from '@angular/core';

const KEY_SIDEBAR = 'bebok.ui.sidebarVisible';
const KEY_SIDEBAR_WIDTH = 'bebok.ui.sidebarWidth';
const KEY_SHELL_SIDEBAR_EXPANDED = 'bebok.ui.shell.sidebarExpanded';
const KEY_RIGHT_DRAWER_OPEN = 'bebok.ui.shell.rightDrawerOpen';
const KEY_RIGHT_DRAWER_PANELS = 'bebok.ui.shell.rightDrawerPanels';
const KEY_RIGHT_DRAWER_WIDTH = 'bebok.ui.shell.rightDrawerWidth';
const KEY_RIGHT_DRAWER_COLLAPSED = 'bebok.ui.shell.rightDrawerCollapsed';
const KEY_EXPAND_TOOL_CALLS = 'bebok.ui.chat.expandToolCalls';

export const DEFAULT_SIDEBAR_WIDTH = 230;
export const MIN_SIDEBAR_WIDTH = 150;
export const MAX_SIDEBAR_WIDTH = 480;

/** Right drawer (redesign): 320px by default (F9-2, was 280), drag-resizable. */
export const DEFAULT_DRAWER_WIDTH = 320;
export const MIN_DRAWER_WIDTH = 280;
export const MAX_DRAWER_WIDTH = 560;

/** The stacked sections of the right drawer - independent, not tabs. */
export interface RightDrawerPanels {
  session: boolean;
  explorer: boolean;
  terminal: boolean;
  /** F6-13: live sub-agent list (Agents panel). */
  agents: boolean;
  /** WP-CHANGES (F6-9): engine-tracked file changes. */
  changes: boolean;
  /** F6-10: markdown/table/link file preview, closed by default. */
  preview: boolean;
  /** WP-BROWSER (F6-19): headless-browser screenshot + URL. */
  browser: boolean;
}

const DEFAULT_PANELS: RightDrawerPanels = {
  session: true,
  explorer: true,
  terminal: false,
  agents: false,
  changes: false,
  preview: false,
  browser: false,
};

export type RightDrawerPanelId = keyof RightDrawerPanels;

export const RIGHT_DRAWER_PANEL_IDS: readonly RightDrawerPanelId[] = [
  'session',
  'explorer',
  'terminal',
  'agents',
  'changes',
  'preview',
  'browser',
];

/** F9-2: collapsed (body hidden, header kept) state per stacked panel. */
export type RightDrawerCollapsed = Record<RightDrawerPanelId, boolean>;

const DEFAULT_COLLAPSED: RightDrawerCollapsed = {
  session: false,
  explorer: false,
  terminal: false,
  agents: false,
  changes: false,
  preview: false,
  browser: false,
};

/**
 * F9-2: one "open this panel and bring it into view" request. Consumers
 * (the drawer component) compare `nonce` with the last one they handled;
 * `at` lets a freshly mounted drawer ignore a request that is long stale.
 */
export interface RightDrawerReveal {
  panel: RightDrawerPanelId;
  nonce: number;
  at: number;
}

function readBool(key: string, fallback: boolean): boolean {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) {
      return fallback;
    }
    return raw === 'true';
  } catch {
    return fallback;
  }
}

function writeString(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* storage unavailable - keep the choice in memory only */
  }
}

function readNumber(key: string, fallback: number, min: number, max: number): number {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) {
      return fallback;
    }
    const value = Number(raw);
    if (!Number.isFinite(value)) {
      return fallback;
    }
    return Math.min(max, Math.max(min, value));
  } catch {
    return fallback;
  }
}

function readWidth(): number {
  return readNumber(KEY_SIDEBAR_WIDTH, DEFAULT_SIDEBAR_WIDTH, MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
}

function readPanels(): RightDrawerPanels {
  try {
    const raw = localStorage.getItem(KEY_RIGHT_DRAWER_PANELS);
    if (!raw) {
      return { ...DEFAULT_PANELS };
    }
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    return {
      session: typeof parsed['session'] === 'boolean' ? parsed['session'] : DEFAULT_PANELS.session,
      explorer:
        typeof parsed['explorer'] === 'boolean' ? parsed['explorer'] : DEFAULT_PANELS.explorer,
      terminal:
        typeof parsed['terminal'] === 'boolean' ? parsed['terminal'] : DEFAULT_PANELS.terminal,
      agents: typeof parsed['agents'] === 'boolean' ? parsed['agents'] : DEFAULT_PANELS.agents,
      changes:
        typeof parsed['changes'] === 'boolean' ? parsed['changes'] : DEFAULT_PANELS.changes,
      preview: typeof parsed['preview'] === 'boolean' ? parsed['preview'] : DEFAULT_PANELS.preview,
      browser:
        typeof parsed['browser'] === 'boolean' ? parsed['browser'] : DEFAULT_PANELS.browser,
    };
  } catch {
    return { ...DEFAULT_PANELS };
  }
}

function readCollapsed(): RightDrawerCollapsed {
  try {
    const raw = localStorage.getItem(KEY_RIGHT_DRAWER_COLLAPSED);
    if (!raw) {
      return { ...DEFAULT_COLLAPSED };
    }
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const out = { ...DEFAULT_COLLAPSED };
    for (const id of RIGHT_DRAWER_PANEL_IDS) {
      if (typeof parsed[id] === 'boolean') {
        out[id] = parsed[id] as boolean;
      }
    }
    return out;
  } catch {
    return { ...DEFAULT_COLLAPSED };
  }
}

@Injectable({ providedIn: 'root' })
export class UiPrefsStore {
  /** Chat sidebar visibility (session summary). */
  readonly sidebarVisible = signal(readBool(KEY_SIDEBAR, true));
  /** Chat sidebar width in px (drag handle). */
  readonly sidebarWidth = signal(readWidth());

  /** Shell: left sidebar expanded (252px) vs collapsed icon rail (60px). */
  readonly sidebarExpanded = signal(readBool(KEY_SHELL_SIDEBAR_EXPANDED, true));
  /** Shell: right drawer visible (chat screen only). */
  readonly rightDrawerOpen = signal(readBool(KEY_RIGHT_DRAWER_OPEN, true));
  /** Shell: which stacked sections of the right drawer are open. */
  readonly rightDrawerPanels = signal<RightDrawerPanels>(readPanels());
  /** Shell: right drawer width in px (pointer-drag resize handle). */
  readonly rightDrawerWidth = signal(
    readNumber(KEY_RIGHT_DRAWER_WIDTH, DEFAULT_DRAWER_WIDTH, MIN_DRAWER_WIDTH, MAX_DRAWER_WIDTH),
  );
  /** Shell (F9-2): which stacked drawer sections are collapsed to their header. */
  readonly rightDrawerCollapsed = signal<RightDrawerCollapsed>(readCollapsed());
  /** Shell (F9-2): latest programmatic "reveal this panel" request (ephemeral). */
  readonly rightDrawerReveal = signal<RightDrawerReveal | null>(null);
  private revealNonce = 0;
  /** Chat: tool calls (and tool-call groups) start expanded (F6-1). */
  readonly expandToolCallsByDefault = signal(readBool(KEY_EXPAND_TOOL_CALLS, false));

  toggleSidebar(): void {
    this.setSidebarVisible(!this.sidebarVisible());
  }

  setSidebarVisible(visible: boolean): void {
    this.sidebarVisible.set(visible);
    writeString(KEY_SIDEBAR, String(visible));
  }

  setSidebarWidth(px: number): void {
    const clamped = Math.min(MAX_SIDEBAR_WIDTH, Math.max(MIN_SIDEBAR_WIDTH, Math.round(px)));
    this.sidebarWidth.set(clamped);
    writeString(KEY_SIDEBAR_WIDTH, String(clamped));
  }

  toggleSidebarExpanded(): void {
    this.setSidebarExpanded(!this.sidebarExpanded());
  }

  setSidebarExpanded(expanded: boolean): void {
    this.sidebarExpanded.set(expanded);
    writeString(KEY_SHELL_SIDEBAR_EXPANDED, String(expanded));
  }

  toggleRightDrawer(): void {
    this.setRightDrawerOpen(!this.rightDrawerOpen());
  }

  setRightDrawerOpen(open: boolean): void {
    this.rightDrawerOpen.set(open);
    writeString(KEY_RIGHT_DRAWER_OPEN, String(open));
  }

  /** Toggle one stacked drawer section; the three are independent. */
  toggleRightDrawerPanel(panel: keyof RightDrawerPanels): void {
    const next = { ...this.rightDrawerPanels(), [panel]: !this.rightDrawerPanels()[panel] };
    this.rightDrawerPanels.set(next);
    writeString(KEY_RIGHT_DRAWER_PANELS, JSON.stringify(next));
  }

  /** F9-2: turn one stacked section on/off explicitly (idempotent). */
  setRightDrawerPanel(panel: RightDrawerPanelId, open: boolean): void {
    if (this.rightDrawerPanels()[panel] === open) {
      return;
    }
    const next = { ...this.rightDrawerPanels(), [panel]: open };
    this.rightDrawerPanels.set(next);
    writeString(KEY_RIGHT_DRAWER_PANELS, JSON.stringify(next));
  }

  setRightDrawerWidth(px: number): void {
    const clamped = Math.min(MAX_DRAWER_WIDTH, Math.max(MIN_DRAWER_WIDTH, Math.round(px)));
    this.rightDrawerWidth.set(clamped);
    writeString(KEY_RIGHT_DRAWER_WIDTH, String(clamped));
  }

  /** F9-2: collapse/expand one section's body; the header stays. */
  toggleRightDrawerPanelCollapsed(panel: RightDrawerPanelId): void {
    this.setRightDrawerPanelCollapsed(panel, !this.rightDrawerCollapsed()[panel]);
  }

  setRightDrawerPanelCollapsed(panel: RightDrawerPanelId, collapsed: boolean): void {
    if (this.rightDrawerCollapsed()[panel] === collapsed) {
      return;
    }
    const next = { ...this.rightDrawerCollapsed(), [panel]: collapsed };
    this.rightDrawerCollapsed.set(next);
    writeString(KEY_RIGHT_DRAWER_COLLAPSED, JSON.stringify(next));
  }

  /**
   * F9-2/F9-3: programmatic open. Shows the drawer, turns the panel's pill
   * on, expands the section and raises a reveal request the drawer answers
   * by scrolling the panel into view. `openDrawer: false` (agent-driven
   * opens such as a browser tool firing) leaves a deliberately closed drawer
   * closed and only arms the panel state for the next time it shows.
   */
  revealRightDrawerPanel(panel: RightDrawerPanelId, openDrawer = true): void {
    if (openDrawer) {
      this.setRightDrawerOpen(true);
    }
    this.setRightDrawerPanel(panel, true);
    this.setRightDrawerPanelCollapsed(panel, false);
    this.revealNonce += 1;
    this.rightDrawerReveal.set({ panel, nonce: this.revealNonce, at: Date.now() });
  }

  toggleExpandToolCallsByDefault(): void {
    this.setExpandToolCallsByDefault(!this.expandToolCallsByDefault());
  }

  setExpandToolCallsByDefault(expand: boolean): void {
    this.expandToolCallsByDefault.set(expand);
    writeString(KEY_EXPAND_TOOL_CALLS, String(expand));
  }
}
