import {
  Component,
  ElementRef,
  OnDestroy,
  effect,
  inject,
  input,
  signal,
  viewChild,
} from '@angular/core';

import { Message } from '../../../core/engine.dtos';

// ─── Threshold constants ───────────────────────────────────────────────
/** Minimum number of messages to show the minimap. */
const MIN_MESSAGES = 3;
/** Minimum px of overflow before showing. */
const MIN_OVERFLOW_PX = 8;
/** Minimum rail height (host element) to render. */
const MIN_RAIL_H = 20;

interface MinimapBlock {
  id: string;
  role: 'user' | 'assistant';
  topPx: number;
  heightPx: number;
  /** Pseudo-line widths as % of rail width (for the "code text" effect). */
  lines: number[];
}

/**
 * Sublime-Text-style scroll minimap for the chat view.
 * Renders a dense, proportional miniature of every message as colored blocks
 * with pseudo-code-lines inside. Supports click-to-jump and drag-to-scroll.
 */
@Component({
  selector: 'app-scroll-minimap',
  template: `
    <div class="minimap" #minimap [class.hidden]="!visible()">
      <!-- backdrop -->
      <div class="minimap-bg"></div>

      <!-- message blocks -->
      <div class="minimap-content" (mousedown)="onRailClick($event)">
        @for (block of blocks(); track block.id) {
          <div
            class="minimap-block"
            [class.user]="block.role === 'user'"
            [style.top.px]="block.topPx"
            [style.height.px]="block.heightPx"
          >
            @for (w of block.lines; track $index) {
              <div class="mm-line" [style.width.%]="w"></div>
            }
          </div>
        }
      </div>

      <!-- viewport indicator -->
      <div
        class="minimap-viewport"
        [style.top.px]="vpTop()"
        [style.height.px]="vpHeight()"
      ></div>
    </div>
  `,
  styles: `
    :host {
      position: absolute;
      top: 0;
      right: 0;
      bottom: 0;
      width: 88px;
      pointer-events: none;
      z-index: 3;
    }

    .minimap {
      position: absolute;
      top: 0;
      bottom: 0;
      left: 0;
      right: 0;
      pointer-events: auto;
      cursor: crosshair;
      overflow: hidden;
    }

    .minimap.hidden {
      opacity: 0;
      pointer-events: none;
    }

    .minimap-bg {
      position: absolute;
      inset: 0;
      background: rgba(18, 20, 24, 0.82);
      border-left: 1px solid rgba(46, 52, 64, 0.6);
    }

    .minimap-content {
      position: absolute;
      top: 0;
      left: 6px;
      right: 6px;
      bottom: 0;
    }

    /* Each message block */
    .minimap-block {
      position: absolute;
      left: 0;
      right: 0;
      border-radius: 1px;
      overflow: hidden;
      display: flex;
      flex-direction: column;
      gap: 1px;
      padding: 1px 2px;
    }

    /* Pseudo "code lines" inside each block */
    .mm-line {
      height: 2px;
      border-radius: 1px;
      opacity: 0.7;
      min-width: 4px;
    }

    /* Assistant messages: muted gray lines */
    .minimap-block:not(.user) .mm-line {
      background: rgba(154, 164, 178, 0.45);
    }

    /* User messages: accent-colored lines */
    .minimap-block.user {
      background: rgba(63, 111, 224, 0.10);
    }
    .minimap-block.user .mm-line {
      background: rgba(91, 140, 255, 0.7);
    }

    /* Viewport indicator — Sublime-style bright rectangle */
    .minimap-viewport {
      position: absolute;
      left: 2px;
      right: 2px;
      border-radius: 2px;
      background: rgba(91, 140, 255, 0.12);
      border: 1px solid rgba(91, 140, 255, 0.35);
      pointer-events: none;
      transition: top 0.05s linear, height 0.05s linear;
    }
  `,
})
export class ScrollMinimapComponent implements OnDestroy {
  readonly messages = input.required<Message[]>();
  readonly scrollTarget = input.required<ElementRef<HTMLElement>>();

  readonly minimap = viewChild<ElementRef<HTMLElement>>('minimap');

  /** Hide when too few messages or content doesn't overflow. */
  readonly visible = signal(false);

  readonly blocks = signal<MinimapBlock[]>([]);
  readonly vpTop = signal(0);
  readonly vpHeight = signal(0);

  private dragActive = false;
  private dragStartY = 0;
  private dragStartScrollTop = 0;
  private rafId = 0;

  private readonly boundOnScroll = this.onScroll.bind(this);

  /** Injected host element — always in the DOM, safe to measure. */
  private readonly hostEl = inject(ElementRef<HTMLElement>);

  /** ResizeObservers — disconnected on destroy. */
  private resizeObs: ResizeObserver | null = null;
  private hostResizeObs: ResizeObserver | null = null;

  /** Tracks which scroll element we've already observed. */
  private observedScrollEl: HTMLElement | null = null;

  constructor() {
    // Re-attach ResizeObserver + trigger recompute when scrollTarget or
    // messages change (e.g. session switch).  This also provides the initial
    // kick-off once both inputs become available.
    effect(() => {
      const msgs = this.messages();
      const el = this.scrollTarget()?.nativeElement;
      if (!el) {
        return;
      }
      // Re-observe when the scroll element changes (session switch).
      if (el !== this.observedScrollEl) {
        this.disconnectResizeObservers();
        if (typeof ResizeObserver !== 'undefined') {
          this.resizeObs = new ResizeObserver(() => this.scheduleRecompute());
          this.resizeObs.observe(el);
          this.hostResizeObs = new ResizeObserver(() => this.scheduleRecompute());
          this.hostResizeObs.observe(this.hostEl.nativeElement);
        }
        this.observedScrollEl = el;
      }
      void msgs.length; // re-trigger on message count change
      this.scheduleRecompute();
    });
  }

  ngOnDestroy(): void {
    const el = this.scrollTarget()?.nativeElement;
    if (el) {
      el.removeEventListener('scroll', this.boundOnScroll);
    }
    if (this.rafId) {
      cancelAnimationFrame(this.rafId);
    }
    this.disconnectResizeObservers();
  }

  /** Public: re-sync geometry (call after scroll, load, session switch). */
  refresh(): void {
    this.scheduleRecompute();
  }

  private disconnectResizeObservers(): void {
    this.resizeObs?.disconnect();
    this.resizeObs = null;
    this.hostResizeObs?.disconnect();
    this.hostResizeObs = null;
  }

  // ─── internal ───────────────────────────────────────────────────────

  private onScroll(): void {
    if (!this.dragActive) {
      this.scheduleRecompute();
    }
  }

  private scheduleRecompute(): void {
    if (this.rafId) {
      cancelAnimationFrame(this.rafId);
    }
    this.rafId = requestAnimationFrame(() => {
      this.rafId = 0;
      this.recompute();
    });
  }

  private recompute(): void {
    const el = this.scrollTarget()?.nativeElement;
    if (!el) {
      return;
    }

    // Attach scroll listener once.
    if (!el.dataset['mmBound']) {
      el.addEventListener('scroll', this.boundOnScroll, { passive: true });
      el.dataset['mmBound'] = '1';
    }

    const msgs = this.messages();

    // Use host element height as the rail height — always in the DOM,
    // no dependency on the potentially-hidden viewChild.
    const railH = this.hostEl.nativeElement.clientHeight;
    const contentH = el.scrollHeight;
    const clientH = el.clientHeight;

    // Decide visibility purely from data conditions.
    if (
      msgs.length < MIN_MESSAGES ||
      contentH <= clientH + MIN_OVERFLOW_PX ||
      railH < MIN_RAIL_H
    ) {
      this.visible.set(false);
      return;
    }
    this.visible.set(true);

    // Scale factor: how many CSS px per real px of content.
    const scale = railH / contentH;

    // Build minimap blocks.
    const newBlocks: MinimapBlock[] = [];
    for (const msg of msgs) {
      const row = el.querySelector<HTMLElement>('#msg-' + CSS.escape(msg.id));
      if (!row) {
        continue;
      }
      const rect = row.getBoundingClientRect();
      const elRect = el.getBoundingClientRect();
      const offsetTop = rect.top - elRect.top + el.scrollTop;
      const blockHeight = Math.max(3, rect.height * scale);

      // Generate pseudo "code lines" based on the message's text content length.
      const lineCount = Math.max(1, Math.min(18, Math.round(blockHeight / 3)));
      const lines: number[] = [];
      for (let i = 0; i < lineCount; i++) {
        const seed = this.hash(msg.id + i);
        const widthPct = 25 + (seed % 75);
        lines.push(widthPct);
      }

      newBlocks.push({
        id: msg.id,
        role: msg.role,
        topPx: offsetTop * scale,
        heightPx: blockHeight,
        lines,
      });
    }
    this.blocks.set(newBlocks);

    // Viewport indicator.
    const vpH = Math.max(12, clientH * scale);
    const scrollRatio = el.scrollTop / (contentH - clientH || 1);
    const vpY = scrollRatio * (railH - vpH);
    this.vpTop.set(vpY);
    this.vpHeight.set(vpH);
  }

  /** Simple deterministic hash for pseudo-random line widths. */
  private hash(s: string): number {
    let h = 0;
    for (let i = 0; i < s.length; i++) {
      h = ((h << 5) - h + s.charCodeAt(i)) | 0;
    }
    return Math.abs(h);
  }

  /** Click on the rail → jump proportionally. */
  onRailClick(event: MouseEvent): void {
    const el = this.scrollTarget()?.nativeElement;
    const minimapEl = this.minimap()?.nativeElement;
    if (!el || !minimapEl) {
      return;
    }

    // If near the viewport thumb, start drag instead.
    const rect = minimapEl.getBoundingClientRect();
    const clickY = event.clientY - rect.top;
    const vpCenter = this.vpTop() + this.vpHeight() / 2;
    if (Math.abs(clickY - vpCenter) < this.vpHeight() * 0.8) {
      this.startDrag(event);
      return;
    }

    // Jump: click position maps proportionally to scroll.
    const ratio = clickY / rect.height;
    const contentH = el.scrollHeight;
    const clientH = el.clientHeight;
    el.scrollTop = ratio * (contentH - clientH) - clientH / 2;
    this.scheduleRecompute();
  }

  /** Drag the viewport indicator to scroll. */
  startDrag(event: MouseEvent): void {
    event.preventDefault();
    event.stopPropagation();
    this.dragActive = true;
    this.dragStartY = event.clientY;
    this.dragStartScrollTop = this.scrollTarget()?.nativeElement?.scrollTop ?? 0;

    const onMove = (ev: MouseEvent): void => {
      if (!this.dragActive) {
        return;
      }
      this.onDragMove(ev);
    };
    const stop = (): void => {
      this.dragActive = false;
      globalThis.removeEventListener('mousemove', onMove);
      globalThis.removeEventListener('mouseup', stop);
    };
    globalThis.addEventListener('mousemove', onMove);
    globalThis.addEventListener('mouseup', stop);
  }

  private onDragMove(ev: MouseEvent): void {
    const el = this.scrollTarget()?.nativeElement;
    const minimapEl = this.minimap()?.nativeElement;
    if (!el || !minimapEl) {
      return;
    }

    const deltaY = ev.clientY - this.dragStartY;
    const contentH = el.scrollHeight;
    const clientH = el.clientHeight;
    const railH = minimapEl.clientHeight;
    const scale = railH / contentH;
    const usableRail = railH - clientH * scale;

    if (usableRail <= 0) {
      return;
    }

    const scrollDelta = (deltaY / usableRail) * (contentH - clientH);
    el.scrollTop = this.dragStartScrollTop + scrollDelta;
    this.scheduleRecompute();
  }
}
