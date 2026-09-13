/**
 * `<app-toast-host>` (F9-5): fixed bottom-right stack of the `ToastStore`
 * messages. Mounted once (bottom of the chat root). Each toast auto-dismisses
 * through the store's timer and can be dismissed early with a click; the
 * container is an `aria-live="polite"` region so screen readers announce
 * new toasts without stealing focus.
 */

import { ChangeDetectionStrategy, Component, inject } from '@angular/core';

import { Toast, ToastStore } from './toast.store';

@Component({
  selector: 'app-toast-host',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="stack" role="status" aria-live="polite" aria-atomic="false" data-testid="toast-host">
      @for (toast of store.toasts(); track toast.id) {
        <button
          type="button"
          class="toast"
          [class.success]="toast.kind === 'success'"
          [class.warning]="toast.kind === 'warning'"
          [class.danger]="toast.kind === 'danger'"
          [attr.data-kind]="toast.kind"
          data-testid="toast"
          (click)="dismiss(toast)"
        >
          <span class="dot" aria-hidden="true"></span>
          <span class="text">{{ toast.text }}</span>
        </button>
      }
    </div>
  `,
  styles: `
    :host {
      display: contents;
    }

    .stack {
      position: fixed;
      right: var(--space-16);
      bottom: var(--space-16);
      z-index: 60;
      display: flex;
      flex-direction: column;
      align-items: flex-end;
      gap: var(--space-8);
      pointer-events: none;
      max-width: min(420px, calc(100vw - 2 * var(--space-16)));
    }

    .toast {
      pointer-events: auto;
      display: flex;
      align-items: flex-start;
      gap: var(--space-8);
      max-width: 100%;
      padding: var(--space-8) var(--space-12);
      border: 1px solid var(--border-strong);
      border-radius: var(--radius-panel);
      background: var(--surface);
      color: var(--text);
      box-shadow: 0 6px 24px rgba(0, 0, 0, 0.28);
      font-size: var(--fs-12-5);
      line-height: 1.45;
      text-align: left;
      cursor: pointer;
      animation: toast-in 160ms ease-out;
    }

    .toast:hover {
      border-color: var(--text-muted);
    }

    .dot {
      flex: none;
      width: 7px;
      height: 7px;
      margin-top: 6px;
      border-radius: 50%;
      background: var(--accent);
    }

    .toast.success .dot {
      background: var(--success);
    }

    .toast.warning .dot {
      background: var(--warning);
    }

    .toast.danger .dot {
      background: var(--danger);
    }

    .text {
      min-width: 0;
      overflow-wrap: anywhere;
    }

    @keyframes toast-in {
      from {
        opacity: 0;
        transform: translateY(6px);
      }
      to {
        opacity: 1;
        transform: none;
      }
    }

    @media (prefers-reduced-motion: reduce) {
      .toast {
        animation: none;
      }
    }
  `,
})
export class ToastHost {
  readonly store = inject(ToastStore);

  dismiss(toast: Toast): void {
    this.store.dismiss(toast.id);
  }
}
