/**
 * "What is Bebok?" screen (F8-4): a static, translated explainer page about
 * the Silesian folklore figure the app is named after. Content lives as
 * plain markdown files under `public/about/bebok.<lang>.md` (built-in static
 * assets, served at the app's own origin - not files read through the
 * engine), rendered through the shared `app-markdown-view` component
 * (WP-PREVIEW) in a centered reading column.
 *
 * Falls back to the English file when the current language has no
 * translation yet, or when the localized fetch fails for any other reason
 * (e.g. a stale build missing a newly-added locale) - see
 * `loadAboutMarkdown` and `about.spec.ts`.
 */

import { ChangeDetectionStrategy, Component, effect, inject, signal } from '@angular/core';
import { RouterLink } from '@angular/router';

import { I18nService } from '../../i18n/i18n.service';
import { MarkdownViewComponent } from '../../ui/markdown-view/markdown-view';

/** One localized markdown fetch attempt: `null` on any non-2xx response or network error. */
async function tryFetch(path: string, fetchImpl: typeof fetch): Promise<string | null> {
  try {
    const res = await fetchImpl(path);
    if (!res.ok) {
      return null;
    }
    return await res.text();
  } catch {
    return null;
  }
}

/**
 * Fetches `about/bebok.<lang>.md`, falling back to `about/bebok.en.md` when
 * the localized file is missing or fails to load. Exported (and taking an
 * injectable `fetchImpl`) so `about.spec.ts` can exercise the fallback path
 * without a real network/DOM.
 */
export async function loadAboutMarkdown(lang: string, fetchImpl: typeof fetch = fetch): Promise<string> {
  const primary = await tryFetch(`about/bebok.${lang}.md`, fetchImpl);
  if (primary !== null) {
    return primary;
  }
  if (lang !== 'en') {
    const fallback = await tryFetch('about/bebok.en.md', fetchImpl);
    if (fallback !== null) {
      return fallback;
    }
  }
  throw new Error(`about/bebok.${lang}.md (and the English fallback) could not be loaded`);
}

@Component({
  selector: 'app-about',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, MarkdownViewComponent],
  templateUrl: './about.html',
  styleUrl: './about.css',
})
export class AboutView {
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly content = signal('');
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);

  constructor() {
    // Re-fetch whenever the language changes (the footer/sidebar selector
    // can be switched while this screen is open).
    effect(() => {
      const lang = this.i18n.lang();
      void this.load(lang);
    });
  }

  private async load(lang: string): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    try {
      const text = await loadAboutMarkdown(lang);
      this.content.set(text);
    } catch (err) {
      this.content.set('');
      this.error.set(err instanceof Error ? err.message : String(err));
    } finally {
      this.loading.set(false);
    }
  }
}
