/**
 * Composer capture controls (WP-M5 / F10-17): camera, gallery and voice for
 * the Capacitor shell.
 *
 * Purely additive on top of the existing composer: a captured photo is
 * emitted as a `File[]` (`files`) that `ChatView.addFiles()` stages exactly
 * like a desktop file pick, and dictation is emitted as text (`transcript`)
 * that the view appends to the draft - it never sends. Every control hides
 * itself when unsupported: camera/gallery when the effective model cannot
 * take images (or the shell has no native camera), the mic when the device
 * has no speech-recognition service.
 */

import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
  output,
  signal,
} from '@angular/core';

import { CameraService, CameraSourceKind } from '../../../core/camera.service';
import { SpeechService } from '../../../core/speech.service';
import { I18nService } from '../../../i18n/i18n.service';

@Component({
  selector: 'app-composer-capture',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (showCamera()) {
      <button
        type="button"
        class="icon-action"
        (click)="capture('camera')"
        [disabled]="disabled() || attachDisabled() || busy()"
        [title]="t('mobile.composer.camera')"
        [attr.aria-label]="t('mobile.composer.camera')"
        data-testid="capture-camera"
      >📷</button>
      <button
        type="button"
        class="icon-action"
        (click)="capture('photos')"
        [disabled]="disabled() || attachDisabled() || busy()"
        [title]="t('mobile.composer.gallery')"
        [attr.aria-label]="t('mobile.composer.gallery')"
        data-testid="capture-gallery"
      >🖼</button>
    }
    @if (showMic()) {
      <button
        type="button"
        class="icon-action mic"
        [class.on]="speech.listening()"
        (click)="dictate()"
        [disabled]="disabled()"
        [title]="speech.listening() ? t('mobile.composer.micStop') : t('mobile.composer.mic')"
        [attr.aria-label]="t('mobile.composer.mic')"
        [attr.aria-pressed]="speech.listening()"
        data-testid="capture-mic"
      >🎙</button>
    }
    @if (error(); as err) {
      <span class="capture-error" role="alert" data-testid="capture-error">{{ err }}</span>
    }
  `,
  styles: [
    `
      :host {
        display: contents;
      }

      .icon-action {
        min-width: 40px;
        min-height: 36px;
        background: transparent;
        border: 1px solid var(--border);
        border-radius: var(--radius-control-sm);
        font-size: 15px;
        line-height: 1;
        padding: 5px 10px;
        color: var(--text-muted);
        cursor: pointer;
      }

      .icon-action:disabled {
        opacity: 0.5;
        cursor: default;
      }

      .icon-action.mic.on {
        color: var(--bg);
        background: var(--accent);
        border-color: var(--accent);
      }

      .capture-error {
        font-size: var(--fs-12);
        color: var(--danger);
      }
    `,
  ],
})
export class ComposerCapture {
  readonly camera = inject(CameraService);
  readonly speech = inject(SpeechService);
  private readonly i18n = inject(I18nService);
  readonly t = this.i18n.t.bind(this.i18n);

  /** Effective model id (`provider/model`), gates the image controls. */
  readonly model = input('');
  /** Composer is loading / a turn blocks input. */
  readonly disabled = input(false);
  /** The attachment limit is reached. */
  readonly attachDisabled = input(false);

  readonly files = output<File[]>();
  readonly transcript = output<string>();

  readonly busy = signal(false);
  readonly error = signal<string | null>(null);

  readonly showCamera = computed(() => this.camera.available && this.camera.imagesSupported(this.model()));
  readonly showMic = computed(() => this.speech.supported() === true);

  constructor() {
    void this.speech.probe();
  }

  async capture(source: CameraSourceKind): Promise<void> {
    if (this.busy()) {
      return;
    }
    this.busy.set(true);
    this.error.set(null);
    try {
      const file = await this.camera.pick(source);
      if (file) {
        this.files.emit([file]);
      }
    } catch (err) {
      this.error.set(this.t('mobile.composer.captureFailed', { error: describe(err) }));
    } finally {
      this.busy.set(false);
    }
  }

  async dictate(): Promise<void> {
    if (this.speech.listening()) {
      await this.speech.stop();
      return;
    }
    this.error.set(null);
    const text = await this.speech.listen(speechLocale(this.i18n.lang()));
    if (text) {
      this.transcript.emit(text);
    } else if (this.speech.error() === 'permission') {
      this.error.set(this.t('mobile.composer.micDenied'));
    } else if (this.speech.error()) {
      this.error.set(this.t('mobile.composer.captureFailed', { error: this.speech.error() }));
    }
  }
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** BCP-47 tag for the recogniser from the UI language (`pl` -> `pl-PL`). */
export function speechLocale(lang: string): string {
  const map: Record<string, string> = {
    en: 'en-US',
    pl: 'pl-PL',
    es: 'es-ES',
    de: 'de-DE',
    fr: 'fr-FR',
    pt: 'pt-PT',
    it: 'it-IT',
    uk: 'uk-UA',
    zh: 'zh-CN',
    ja: 'ja-JP',
    ko: 'ko-KR',
    hi: 'hi-IN',
  };
  return map[lang] ?? navigator.language ?? 'en-US';
}
