/**
 * i18n service: a tiny signal-based translator (no Angular localize build step).
 *
 * The selected language is a signal, so any template that calls `t()` re-renders
 * when the language changes (zoneless change detection tracks the read).
 * Selection is persisted to localStorage and defaults to English.
 */

import { Injectable, signal } from '@angular/core';

import {
  DEFAULT_LANGUAGE,
  LANGUAGES,
  translations,
  type Language,
  type LanguageOption,
  type MessageKey,
} from './index';

const STORAGE_KEY = 'bebok.lang';

@Injectable({ providedIn: 'root' })
export class I18nService {
  private readonly current = signal<Language>(this.loadInitial());

  /** Reactive current language code. */
  readonly lang = this.current.asReadonly();

  /** Translate a key, optionally interpolating `{name}` placeholders. */
  t(key: MessageKey, params?: Record<string, unknown>): string {
    const dict = translations[this.current()] ?? translations.en;
    let message: string = dict[key] ?? translations.en[key] ?? key;
    if (params) {
      for (const [name, value] of Object.entries(params)) {
        message = message.replaceAll(`{${name}}`, String(value));
      }
    }
    return message;
  }

  setLanguage(lang: Language): void {
    this.current.set(lang);
    try {
      localStorage.setItem(STORAGE_KEY, lang);
    } catch {
      /* localStorage unavailable - keep in memory only */
    }
  }

  /** Language list for the selector (self-named). */
  languages(): LanguageOption[] {
    return LANGUAGES;
  }

  private loadInitial(): Language {
    try {
      const saved = localStorage.getItem(STORAGE_KEY);
      if (saved && saved in translations) {
        return saved as Language;
      }
    } catch {
      /* ignore */
    }
    return DEFAULT_LANGUAGE;
  }
}
