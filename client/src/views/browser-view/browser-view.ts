/**
 * Browser viewer window (WP-BROWSER2 / F7-6): `/browser-view?session=<id>`.
 *
 * Rendered without the app shell in its own window (Tauri `WebviewWindow` or
 * a `window.open` popup). It mirrors the agent's browser for one session:
 *
 * * frames arrive as `browser.frame` SSE events (at most 2 fps while a
 *   `browser_*` tool is running) and on demand via `GET
 *   /session/{id}/browser/frame` (first paint, and a slow keep-alive poll
 *   that also tells the engine a viewer is watching);
 * * the toolbar drives the page through `POST /session/{id}/browser/{action}`
 *   - the engine executes the very same tools the model uses, under the same
 *   permission rules (a `deny` rule comes back as HTTP 403 and is shown);
 * * a click on the frame is translated to page coordinates (the image is
 *   scaled to fit the window, the engine wants CSS pixels of the page).
 *
 * The page connects to the engine on its own (`EngineClient.connect()`), so
 * it works in a fresh window with nothing handed over but the session id.
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  OnInit,
  computed,
  inject,
  signal,
  viewChild,
} from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { BrowserAction, BrowserFrame, BrowserState, EngineEvent } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { I18nService } from '../../i18n/i18n.service';
import { isLinkable } from '../../ui/right-drawer/panels/browser-panel';

/** Keep-alive poll: below the engine's 120 s viewer TTL, cheap (one JPEG). */
export const KEEPALIVE_MS = 45_000;

/**
 * Map a click inside the rendered `<img>` (object-fit: contain) to page
 * coordinates. Returns `null` for clicks on the letterbox margins.
 */
export function clickToPage(
  box: { width: number; height: number },
  natural: { width: number; height: number },
  offset: { x: number; y: number },
): { x: number; y: number } | null {
  if (box.width <= 0 || box.height <= 0 || natural.width <= 0 || natural.height <= 0) {
    return null;
  }
  const scale = Math.min(box.width / natural.width, box.height / natural.height);
  const drawnW = natural.width * scale;
  const drawnH = natural.height * scale;
  const left = (box.width - drawnW) / 2;
  const top = (box.height - drawnH) / 2;
  const x = (offset.x - left) / scale;
  const y = (offset.y - top) / scale;
  if (x < 0 || y < 0 || x > natural.width || y > natural.height) {
    return null;
  }
  return { x: Math.round(x), y: Math.round(y) };
}

/** Add `https://` to a bare host the user typed into the URL bar. */
export function normalizeTypedUrl(raw: string): string {
  const s = raw.trim();
  if (!s) {
    return '';
  }
  if (/^[a-z][a-z0-9+.-]*:/i.test(s)) {
    return s;
  }
  return `https://${s}`;
}

@Component({
  selector: 'app-browser-view',
  imports: [FormsModule],
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './browser-view.html',
  styleUrl: './browser-view.css',
})
export class BrowserView implements OnInit {
  private readonly i18n = inject(I18nService);
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly route = inject(ActivatedRoute);
  private readonly destroyRef = inject(DestroyRef);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly sessionId = signal('');
  readonly state = signal<BrowserState | null>(null);
  readonly frame = signal<BrowserFrame | null>(null);
  readonly error = signal<string | null>(null);
  /** Result text of the last user action (`ok:false` tool messages too). */
  readonly notice = signal<string | null>(null);
  readonly busy = signal(false);
  readonly connecting = signal(true);
  /** URL bar draft (follows the page until the user edits it). */
  readonly urlDraft = signal('');
  readonly urlEdited = signal(false);
  readonly typeDraft = signal('');
  readonly typeSubmit = signal(false);

  readonly frameSrc = computed(() => {
    const f = this.frame();
    return f ? `data:${f.media_type};base64,${f.data}` : null;
  });
  readonly currentUrl = computed(() => this.frame()?.url ?? this.state()?.url ?? '');
  readonly currentTitle = computed(() => this.frame()?.title ?? this.state()?.title ?? '');
  readonly open = computed(() => this.state()?.open ?? this.frame() !== null);
  readonly headed = computed(() => this.frame()?.headed ?? this.state()?.headed ?? false);
  readonly linkable = computed(() => isLinkable(this.currentUrl()));

  private readonly img = viewChild<ElementRef<HTMLImageElement>>('frameImg');
  private lastSeq = -1;
  private keepAlive: number | undefined;

  constructor() {
    const unsubscribe = this.events.onEvent((ev) => this.handleEvent(ev));
    this.destroyRef.onDestroy(() => {
      unsubscribe();
      if (this.keepAlive !== undefined) {
        window.clearInterval(this.keepAlive);
      }
    });
  }

  async ngOnInit(): Promise<void> {
    const id = this.route.snapshot.queryParamMap.get('session') ?? '';
    this.sessionId.set(id);
    if (!id) {
      this.error.set(this.t('browserView.noSession'));
      this.connecting.set(false);
      return;
    }
    if (typeof document !== 'undefined') {
      document.title = `${this.t('browserView.title')} · Bebok`;
    }
    if (!this.engine.connected()) {
      try {
        await this.engine.connect();
      } catch (err) {
        this.error.set(this.describe(err));
        this.connecting.set(false);
        return;
      }
    }
    this.events.start();
    this.connecting.set(false);
    await this.refresh();
    this.keepAlive = window.setInterval(() => void this.pollFrame(), KEEPALIVE_MS);
  }

  /** Re-read state + one frame (initial paint, reconnects, after actions). */
  async refresh(): Promise<void> {
    const id = this.sessionId();
    if (!id) {
      return;
    }
    try {
      const state = await this.engine.browserState(id);
      this.state.set(state);
      this.error.set(null);
      if (!this.urlEdited()) {
        this.urlDraft.set(state.url);
      }
    } catch (err) {
      this.error.set(this.describe(err));
      return;
    }
    await this.pollFrame();
  }

  /** One on-demand frame; a 404 just means no browser is open (not an error). */
  async pollFrame(): Promise<void> {
    const id = this.sessionId();
    if (!id) {
      return;
    }
    try {
      const frame = await this.engine.browserFrame(id);
      this.applyFrame(frame);
    } catch (err) {
      if (!/-> 404/.test(String(err instanceof Error ? err.message : err))) {
        this.error.set(this.describe(err));
      }
    }
  }

  handleEvent(event: EngineEvent): void {
    if (event.sessionID !== this.sessionId()) {
      return;
    }
    switch (event.type) {
      case 'browser.frame': {
        const props = event.properties as unknown as BrowserFrame | undefined;
        if (props && typeof props.data === 'string') {
          this.applyFrame(props);
        }
        break;
      }
      case 'browser.closed':
      case 'session.deleted':
        this.state.update((s) => (s ? { ...s, open: false, headed: null } : s));
        this.frame.set(null);
        this.lastSeq = -1;
        break;
      case 'session.updated':
        if (event.properties?.['running'] === false) {
          // Turn ended: the last streamed frame may predate the final render.
          void this.pollFrame();
        }
        break;
      default:
        break;
    }
  }

  /** Accept a frame unless it is older than the newest one already shown. */
  applyFrame(frame: BrowserFrame): void {
    if (frame.seq < this.lastSeq) {
      return;
    }
    this.lastSeq = frame.seq;
    this.frame.set(frame);
    this.state.update((s) =>
      s ? { ...s, open: true, headed: frame.headed, url: frame.url, title: frame.title } : s,
    );
    if (!this.urlEdited()) {
      this.urlDraft.set(frame.url);
    }
  }

  // --- toolbar -------------------------------------------------------------

  onUrlInput(value: string): void {
    this.urlDraft.set(value);
    this.urlEdited.set(value !== this.currentUrl());
  }

  async navigate(): Promise<void> {
    const url = normalizeTypedUrl(this.urlDraft());
    if (!url) {
      return;
    }
    this.urlEdited.set(false);
    await this.action('navigate', { url });
  }

  async back(): Promise<void> {
    await this.action('back');
  }

  async forward(): Promise<void> {
    await this.action('forward');
  }

  async reload(): Promise<void> {
    await this.action('reload');
  }

  async typeText(): Promise<void> {
    const text = this.typeDraft();
    if (!text) {
      return;
    }
    const ok = await this.action('type', { text, submit: this.typeSubmit() });
    if (ok) {
      this.typeDraft.set('');
    }
  }

  async closeBrowser(): Promise<void> {
    await this.action('close');
    this.frame.set(null);
    this.lastSeq = -1;
    await this.refresh();
  }

  /** A click on the mirrored page becomes a `browser_click` at page coordinates. */
  onFrameClick(event: MouseEvent): void {
    const el = this.img()?.nativeElement;
    if (!el || this.busy()) {
      return;
    }
    const rect = el.getBoundingClientRect();
    // The engine wants CSS pixels of the page. Frames carry the CSS viewport
    // size (the image itself may be scaled on HiDPI screens); fall back to
    // the image's own size when a frame did not say.
    const f = this.frame();
    const natural =
      f && f.width > 0 && f.height > 0
        ? { width: f.width, height: f.height }
        : { width: el.naturalWidth, height: el.naturalHeight };
    const point = clickToPage({ width: rect.width, height: rect.height }, natural, {
      x: event.clientX - rect.left,
      y: event.clientY - rect.top,
    });
    if (!point) {
      return;
    }
    void this.action('click', { x: point.x, y: point.y });
  }

  /**
   * Run one user action. Resolves to `true` when the engine executed it
   * successfully; tool-level failures (`ok: false`) and HTTP errors (403 for a
   * `deny` rule) are shown in the status line.
   */
  async action(action: BrowserAction, body: Record<string, unknown> = {}): Promise<boolean> {
    const id = this.sessionId();
    if (!id || this.busy()) {
      return false;
    }
    this.busy.set(true);
    this.notice.set(null);
    this.error.set(null);
    try {
      const result = await this.engine.browserAction(id, action, body);
      if (result.ok === false) {
        this.notice.set(result.text ?? this.t('browserView.actionFailed'));
        return false;
      }
      if (typeof result.url === 'string') {
        this.state.update((s) =>
          s ? { ...s, open: true, url: result.url ?? s.url, title: result.title ?? s.title } : s,
        );
        if (!this.urlEdited()) {
          this.urlDraft.set(result.url);
        }
      }
      return true;
    } catch (err) {
      const msg = this.describe(err);
      this.error.set(/-> 403/.test(msg) ? `${this.t('browserView.denied')} ${msg}` : msg);
      return false;
    } finally {
      this.busy.set(false);
      // The stream sends a frame when the tool finishes; fetch one anyway in
      // case nobody was streaming (e.g. first action after the window opened).
      void this.pollFrame();
    }
  }

  private describe(err: unknown): string {
    return err instanceof Error ? err.message : String(err);
  }
}
