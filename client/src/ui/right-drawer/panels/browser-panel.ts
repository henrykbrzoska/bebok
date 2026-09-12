/**
 * Right-drawer "Browser" panel (WP-BROWSER / F6-19).
 *
 * Shows what the agent's headless browser last did in the open session: the
 * most recent `browser_screenshot` image, the current URL/title (from the
 * `structured` JSON every `browser_*` tool result carries) and an
 * "Open in my browser" link. There is no push channel of its own: the panel
 * is a pure projection of `ChatSessionStore.messages`, which the chat view
 * already patches from `message.part.updated` SSE events, so it updates the
 * moment a tool result lands - no polling, no manual refresh.
 *
 * Pairing rule for screenshots: the engine inserts the image part directly
 * after its `browser_screenshot` tool part (named `browser_screenshot:<call
 * id>`), so "tool part followed by an image part" is the lookup.
 */

import { ChangeDetectionStrategy, Component, computed, inject } from '@angular/core';

import { ImagePart, Message, Part, ToolPart } from '../../../core/engine.dtos';
import { I18nService } from '../../../i18n/i18n.service';
import { ChatSessionStore } from '../../../views/chat/chat-session.store';

/** What a `browser_*` tool reports in `structured` (see bebok-tools/browser). */
interface BrowserToolState {
  url?: string;
  title?: string;
}

export interface BrowserScreenshot {
  mediaType: string;
  /** Raw base64 (no `data:` prefix). */
  data: string;
  url: string;
  title: string;
}

export interface BrowserLocation {
  url: string;
  title: string;
  /** Name of the tool that reported it, e.g. `browser_open`. */
  tool: string;
  /** `true` when the reporting call failed (its error text is `error`). */
  error?: string;
}

function isBrowserTool(part: Part): part is ToolPart {
  return part.type === 'tool' && part.name.startsWith('browser_');
}

function structuredOf(part: ToolPart): BrowserToolState | null {
  if (part.state.state !== 'completed') {
    return null;
  }
  const s = part.state.structured as unknown;
  if (!s || typeof s !== 'object') {
    return null;
  }
  const rec = s as Record<string, unknown>;
  return {
    url: typeof rec['url'] === 'string' ? rec['url'] : undefined,
    title: typeof rec['title'] === 'string' ? rec['title'] : undefined,
  };
}

/** Newest screenshot in the transcript: a completed `browser_screenshot`
 * tool part immediately followed by an image part. */
export function latestScreenshot(messages: readonly Message[]): BrowserScreenshot | null {
  for (let m = messages.length - 1; m >= 0; m--) {
    const parts = messages[m].parts;
    for (let i = parts.length - 2; i >= 0; i--) {
      const part = parts[i];
      const next = parts[i + 1];
      if (
        part.type === 'tool' &&
        part.name === 'browser_screenshot' &&
        part.state.state === 'completed' &&
        next.type === 'image'
      ) {
        const img = next as ImagePart;
        const state = structuredOf(part);
        return {
          mediaType: img.media_type,
          data: img.data,
          url: state?.url ?? '',
          title: state?.title ?? '',
        };
      }
    }
  }
  return null;
}

/** Newest `browser_*` result that reported a URL (or the newest error). */
export function latestLocation(messages: readonly Message[]): BrowserLocation | null {
  for (let m = messages.length - 1; m >= 0; m--) {
    const parts = messages[m].parts;
    for (let i = parts.length - 1; i >= 0; i--) {
      const part = parts[i];
      if (!isBrowserTool(part)) {
        continue;
      }
      if (part.state.state === 'error') {
        return { url: '', title: '', tool: part.name, error: part.state.error };
      }
      const state = structuredOf(part);
      if (state?.url) {
        return { url: state.url, title: state.title ?? '', tool: part.name };
      }
    }
  }
  return null;
}

/** Only http(s) URLs are offered as an outbound link (never `file:` etc.). */
export function isLinkable(url: string): boolean {
  return /^https?:\/\//i.test(url);
}

@Component({
  selector: 'app-browser-panel',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (!session.meta()) {
      <div class="empty">{{ t('drawer.noSession') }}</div>
    } @else if (!location() && !screenshot()) {
      <div class="empty">{{ t('browser.none') }}</div>
    } @else {
      <div class="panel-body">
        @if (location(); as loc) {
          <div class="location" data-testid="browser-location">
            @if (loc.error) {
              <div class="error" [title]="loc.error">
                <span class="tool">{{ loc.tool }}</span> {{ loc.error }}
              </div>
            } @else {
              @if (loc.title) {
                <div class="title" [title]="loc.title">{{ loc.title }}</div>
              }
              <div class="url" [title]="loc.url">{{ loc.url }}</div>
              @if (isLinkable(loc.url)) {
                <a
                  class="open"
                  data-testid="browser-open-link"
                  [href]="loc.url"
                  target="_blank"
                  rel="noopener noreferrer"
                  >{{ t('browser.openExternal') }} ↗</a
                >
              }
            }
          </div>
        }

        @if (screenshot(); as shot) {
          <figure class="shot">
            <img
              data-testid="browser-screenshot"
              [src]="'data:' + shot.mediaType + ';base64,' + shot.data"
              [alt]="t('browser.screenshotAlt')"
            />
            @if (shot.url && shot.url !== location()?.url) {
              <figcaption [title]="shot.url">{{ t('browser.screenshotOf') }} {{ shot.url }}</figcaption>
            }
          </figure>
        } @else {
          <div class="empty small">{{ t('browser.noScreenshot') }}</div>
        }
      </div>
    }
  `,
  styles: [
    `
      .empty {
        padding: var(--space-16);
        font-size: var(--fs-11-5);
        color: var(--text-faint);
      }

      .empty.small {
        padding: var(--space-8) var(--space-10);
      }

      .panel-body {
        display: flex;
        flex-direction: column;
        gap: var(--space-8);
        padding: var(--space-8) var(--space-10) var(--space-12);
        min-width: 0;
      }

      .location {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
      }

      .title {
        font-size: var(--fs-12-5);
        font-weight: 600;
        color: var(--text);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .url {
        font-family: var(--font-mono);
        font-size: var(--fs-11);
        color: var(--text-faint);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .open {
        align-self: flex-start;
        margin-top: 2px;
        font-size: var(--fs-11-5);
        color: var(--accent);
        text-decoration: none;
      }

      .open:hover {
        text-decoration: underline;
      }

      .error {
        font-size: var(--fs-11);
        color: var(--danger);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .tool {
        font-family: var(--font-mono);
        color: var(--text-faint);
      }

      .shot {
        margin: 0;
        min-width: 0;
      }

      .shot img {
        display: block;
        width: 100%;
        height: auto;
        border: 1px solid var(--border);
        border-radius: var(--radius-control-sm);
        background: var(--surface-2);
      }

      .shot figcaption {
        margin-top: 2px;
        font-family: var(--font-mono);
        font-size: 10px;
        color: var(--text-faint);
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
    `,
  ],
})
export class BrowserPanel {
  private readonly i18n = inject(I18nService);
  readonly session = inject(ChatSessionStore);

  readonly t = this.i18n.t.bind(this.i18n);

  /** Latest screenshot in the open session's transcript. */
  readonly screenshot = computed(() => latestScreenshot(this.session.messages()));
  /** Where the headless browser currently is (newest `browser_*` result). */
  readonly location = computed(() => latestLocation(this.session.messages()));

  readonly isLinkable = isLinkable;
}
