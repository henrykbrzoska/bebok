/**
 * Chrome preferences: persisted layout state for the app shell.
 *
 * Everything here is client-side only (localStorage) - the engine is never
 * involved. Two generations of keys live side by side:
 *
 * - `sidebarVisible` / `sidebarWidth` belong to the chat session sidebar
 *   (`ui/session-sidebar`), untouched by the redesign.
 * - `sidebarExpanded`, `rightDrawerOpen`, `rightDrawerPanels`,
 *   `rightDrawerWidth` and `density` are the WP-SHELL redesign state
 *   (F1-3 / F1-12). `density` replaces the old `topbarCompact` flag.
 * - `expandToolCallsByDefault` is the chat transcript preference (F6-1):
 *   when on, every tool call (and every grouped run of tool calls) starts
 *   expanded; when off, only the first tool call of a turn does.
 */

import { Injectable, signal } from '@angular/core';

const KEY_SIDEBAR = 'bebok.ui.sidebarVisible';
const KEY_SIDEBAR_WIDTH = 'bebok.ui.sidebarWidth';
const KEY_SHELL_SIDEBAR_EXPANDED = 'bebok.ui.shell.sidebarExpanded';
const KEY_RIGHT_DRAWER_OPEN = 'bebok.ui.shell.rightDrawerOpen';
const KEY_RIGHT_DRAWER_PANELS = 'bebok.ui.shell.rightDrawerPanels';
const KEY_RIGHT_DRAWER_WIDTH = 'bebok.ui.shell.rightDrawerWidth';
const KEY_DENSITY = 'bebok.ui.shell.density';
const KEY_EXPAND_TOOL_CALLS = 'bebok.ui.chat.expandToolCalls';

export const DEFAULT_SIDEBAR_WIDTH = 230;
export const MIN_SIDEBAR_WIDTH = 150;
export const MAX_SIDEBAR_WIDTH = 480;

/** Right drawer (redesign): fixed 280px by default, drag-resizable. */
export const DEFAULT_DRAWER_WIDTH = 280;
export const MIN_DRAWER_WIDTH = 220;
export const MAX_DRAWER_WIDTH = 560;

export type Density = 'comfortable' | 'compact';

/** The three stacked sections of the right drawer - independent, not tabs. */
export interface RightDrawerPanels {
  session: boolean;
  explorer: boolean;
  terminal: boolean;
  /** F6-13: live sub-agent list (Agents panel). */
  agents: boolean;
}

const DEFAULT_PANELS: RightDrawerPanels = {
  session: true,
  explorer: true,
  terminal: false,
  agents: false,
};

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

function readDensity(): Density {
  try {
    return localStorage.getItem(KEY_DENSITY) === 'compact' ? 'compact' : 'comfortable';
  } catch {
    return 'comfortable';
  }
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
    };
  } catch {
    return { ...DEFAULT_PANELS };
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
  /** Shell: spacing density for lists/settings. */
  readonly density = signal<Density>(readDensity());
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

  setRightDrawerWidth(px: number): void {
    const clamped = Math.min(MAX_DRAWER_WIDTH, Math.max(MIN_DRAWER_WIDTH, Math.round(px)));
    this.rightDrawerWidth.set(clamped);
    writeString(KEY_RIGHT_DRAWER_WIDTH, String(clamped));
  }

  toggleDensity(): void {
    this.setDensity(this.density() === 'compact' ? 'comfortable' : 'compact');
  }

  setDensity(density: Density): void {
    this.density.set(density);
    writeString(KEY_DENSITY, density);
  }

  toggleExpandToolCallsByDefault(): void {
    this.setExpandToolCallsByDefault(!this.expandToolCallsByDefault());
  }

  setExpandToolCallsByDefault(expand: boolean): void {
    this.expandToolCallsByDefault.set(expand);
    writeString(KEY_EXPAND_TOOL_CALLS, String(expand));
  }
}
