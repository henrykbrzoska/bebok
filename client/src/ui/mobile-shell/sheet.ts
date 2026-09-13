/**
 * Bottom sheet (WP-M2 / F10-8): the phone's modal surface.
 *
 * Slides up from the bottom edge over a dimmed backdrop; tapping the
 * backdrop, pressing Escape or dragging the handle down past `DISMISS_PX`
 * emits `close` (the parent owns `open`, so a non-dismissible sheet simply
 * ignores the output). WP-M6 renders permission prompts and session pickers
 * in it; WP-M5 the onboarding steps.
 *
 * Usage:
 *   <app-sheet [open]="open()" [title]="t('…')" (close)="open.set(false)">
 *     …content…
 *   </app-sheet>
 */

import {
  ChangeDetectionStrategy,
  Component,
  HostListener,
  computed,
  input,
  output,
  signal,
} from '@angular/core';

/** Drag distance (px) past which releasing the handle dismisses the sheet. */
export const DISMISS_PX = 80;

@Component({
  selector: 'app-sheet',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (open()) {
      <div class="sheet-backdrop" data-testid="sheet-backdrop" (click)="requestClose()"></div>
      <section
        class="sheet"
        role="dialog"
        aria-modal="true"
        [attr.aria-label]="title() || null"
        [class.dragging]="dragging()"
        [style.transform]="transform()"
        data-testid="sheet"
      >
        <div
          class="sheet-handle"
          data-testid="sheet-handle"
          (pointerdown)="onPointerDown($event)"
          (pointermove)="onPointerMove($event)"
          (pointerup)="onPointerUp($event)"
          (pointercancel)="onPointerUp($event)"
        >
          <span class="grip" aria-hidden="true"></span>
        </div>
        @if (title()) {
          <h2 class="sheet-title">{{ title() }}</h2>
        }
        <div class="sheet-body">
          <ng-content />
        </div>
      </section>
    }
  `,
  styles: [
    `
      :host {
        display: contents;
      }

      .sheet-backdrop {
        position: fixed;
        inset: 0;
        background: rgba(0, 0, 0, 0.55);
        z-index: 60;
      }

      .sheet {
        position: fixed;
        left: 0;
        right: 0;
        bottom: 0;
        z-index: 61;
        max-height: min(85dvh, 85vh);
        display: flex;
        flex-direction: column;
        background: var(--surface);
        border-top: 1px solid var(--border-strong);
        border-radius: 12px 12px 0 0;
        padding-bottom: env(safe-area-inset-bottom, 0px);
        box-shadow: 0 -8px 32px rgba(0, 0, 0, 0.45);
        transition: transform 160ms ease-out;
        animation: sheet-in 180ms ease-out;
      }

      .sheet.dragging {
        transition: none;
      }

      @keyframes sheet-in {
        from {
          transform: translateY(100%);
        }
        to {
          transform: translateY(0);
        }
      }

      .sheet-handle {
        flex: none;
        display: flex;
        justify-content: center;
        padding: 10px 0 6px;
        touch-action: none;
        cursor: grab;
      }

      .grip {
        width: 36px;
        height: 4px;
        border-radius: 2px;
        background: var(--border-strong);
      }

      .sheet-title {
        flex: none;
        margin: 0;
        padding: 4px var(--space-16) var(--space-8);
        font-size: var(--fs-14);
        font-weight: 600;
        color: var(--text);
      }

      .sheet-body {
        flex: 1 1 auto;
        min-height: 0;
        overflow-y: auto;
        padding: 0 var(--space-16) var(--space-16);
      }
    `,
  ],
})
export class Sheet {
  readonly open = input(false);
  readonly title = input('');
  /** When false, backdrop taps / Escape / drag do not emit `close`. */
  readonly dismissible = input(true);
  readonly close = output<void>();

  readonly dragging = signal(false);
  private readonly dragY = signal(0);
  private startY: number | null = null;

  readonly transform = computed(() => {
    const y = this.dragY();
    return y > 0 ? `translateY(${y}px)` : null;
  });

  @HostListener('document:keydown.escape')
  onEscape(): void {
    if (this.open()) {
      this.requestClose();
    }
  }

  requestClose(): void {
    if (this.dismissible()) {
      this.close.emit();
    }
  }

  onPointerDown(ev: PointerEvent): void {
    this.startY = ev.clientY;
    this.dragging.set(true);
    try {
      (ev.target as HTMLElement).setPointerCapture?.(ev.pointerId);
    } catch {
      /* pointer capture unsupported */
    }
  }

  onPointerMove(ev: PointerEvent): void {
    if (this.startY === null) {
      return;
    }
    this.dragY.set(Math.max(0, ev.clientY - this.startY));
  }

  onPointerUp(_ev: PointerEvent): void {
    if (this.startY === null) {
      return;
    }
    const travelled = this.dragY();
    this.startY = null;
    this.dragging.set(false);
    this.dragY.set(0);
    if (travelled >= DISMISS_PX) {
      this.requestClose();
    }
  }
}
