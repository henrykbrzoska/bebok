/**
 * Thin DI wrapper around `code-highlight.ts`'s pure functions (F7-4), for the
 * component call sites (`views/explorer`, `ui/diff-view`) that already inject
 * their other collaborators. The markdown renderers (`core/markdown.ts`,
 * `ui/markdown-view/markdown-view.render.ts`) are plain functions with no
 * injection context, so they import the pure module directly instead - both
 * paths share the exact same grammar cache and `highlightLanguagesVersion`
 * signal underneath.
 */

import { Injectable } from '@angular/core';

import {
  HighlightOptions,
  HighlightResult,
  highlightBlockHtml,
  highlightCode,
  resolveLanguage,
} from './code-highlight';

@Injectable({ providedIn: 'root' })
export class CodeHighlightService {
  /** Token-highlighted HTML for `code` (no wrapping `<pre>`/`<code>`). */
  highlight(code: string, options?: HighlightOptions): HighlightResult {
    return highlightCode(code, options);
  }

  /** `highlight()`, wrapped in `<pre><code class="hljs language-...">`. */
  highlightBlock(code: string, options?: HighlightOptions): string {
    return highlightBlockHtml(code, options);
  }

  /** Resolve a fence hint / language name and/or filename to a known language. */
  resolveLanguage(hint?: string | null, filename?: string | null): string | null {
    return resolveLanguage(hint, filename);
  }
}
