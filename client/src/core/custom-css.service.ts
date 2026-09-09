/**
 * Custom CSS injection (Task 6, client side of `ui.customCss`).
 *
 * The engine serves `ui.customCss` (+ `ui.customCssFiles`) as plain text via
 * `GET /config` and persists it via `PUT /config`; it never executes it.
 * This service injects the CSS as a single `<style id="bebok-custom-css">`
 * element in `document.head` (replaced on every sync). File entries are read
 * through the engine (`GET /fs/file`) and concatenated after the inline CSS.
 * No eval, no script execution — text only.
 */

import { Injectable, inject } from '@angular/core';

import { EngineClient } from './engine-client.service';
import type { ResolvedConfig } from './engine.dtos';

const STYLE_ID = 'bebok-custom-css';
const MAX_CSS_CHARS = 200 * 1024;

@Injectable({ providedIn: 'root' })
export class CustomCssService {
  private readonly engine = inject(EngineClient);
  private lastDirectory: string | null = null;

  /** Fetch `ui` for `directory` and (re)apply it. Safe to call repeatedly. */
  async sync(directory: string): Promise<void> {
    this.lastDirectory = directory;
    try {
      const cfg = await this.engine.getConfig(directory);
      const files = await this.loadCssFiles(directory, cfg.config);
      this.apply(this.combine(cfg.config, files));
    } catch {
      /* theming is non-critical: keep the last applied style */
    }
  }

  /** Re-apply for the last directory (e.g. after `config.changed`). */
  async resync(): Promise<void> {
    const dir = this.lastDirectory;
    if (dir) {
      await this.sync(dir);
    }
  }

  /** Replace the injected style element (plain text only). */
  apply(css: string): void {
    const doc = globalThis.document;
    if (!doc) {
      return;
    }
    const text = css.length > MAX_CSS_CHARS ? css.slice(0, MAX_CSS_CHARS) : css;
    let el = doc.getElementById(STYLE_ID) as HTMLStyleElement | null;
    if (!el) {
      el = doc.createElement('style');
      el.id = STYLE_ID;
      el.setAttribute('data-source', 'bebok-ui-customCss');
      doc.head.appendChild(el);
    }
    el.textContent = text;
  }

  /** Remove the injected style element. */
  clear(): void {
    globalThis.document?.getElementById(STYLE_ID)?.remove();
  }

  private combine(config: ResolvedConfig, filesCss: string): string {
    const ui = config.ui as
      | {
          customCss?: string;
          custom_css?: string;
          customCssFiles?: string[];
          custom_css_files?: string[];
        }
      | undefined;
    const inline = ui?.customCss ?? ui?.custom_css ?? '';
    return [inline, filesCss].filter((s) => s.trim().length > 0).join('\n');
  }

  private async loadCssFiles(directory: string, config: ResolvedConfig): Promise<string> {
    const ui = config.ui as
      | { customCssFiles?: string[]; custom_css_files?: string[] }
      | undefined;
    const list = ui?.customCssFiles ?? ui?.custom_css_files ?? [];
    if (!Array.isArray(list) || list.length === 0) {
      return '';
    }
    const out: string[] = [];
    for (const raw of list.slice(0, 32)) {
      if (typeof raw !== 'string' || !raw.trim()) {
        continue;
      }
      // Only relative CSS-ish paths; never absolute URLs / escapes.
      const p = raw.trim();
      if (p.startsWith('http://') || p.startsWith('https://') || p.startsWith('/') || p.includes('..')) {
        continue;
      }
      try {
        const res = await this.engine.fsFile(directory, p);
        if (res.content) {
          out.push(`/* ${p} */\n${res.content}`);
        }
      } catch {
        /* missing file: skip */
      }
    }
    return out.join('\n');
  }
}
