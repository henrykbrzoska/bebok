/**
 * i18n catalog: translation dictionaries + language metadata.
 *
 * English is the reference/default dictionary; every other language must
 * provide every key (`Record<MessageKey, string>`), so a missing key is a
 * compile-time error rather than a runtime fallback.
 */

import { en, type MessageKey } from './en';
import { de } from './de';
import { es } from './es';
import { fr } from './fr';
import { hi } from './hi';
import { it } from './it';
import { ja } from './ja';
import { ko } from './ko';
import { pl } from './pl';
import { pt } from './pt';
import { uk } from './uk';
import { zh } from './zh';

export type { MessageKey };

export type Language =
  | 'en'
  | 'pl'
  | 'es'
  | 'de'
  | 'fr'
  | 'pt'
  | 'it'
  | 'uk'
  | 'zh'
  | 'ja'
  | 'ko'
  | 'hi';

export type TranslationDict = Record<MessageKey, string>;

export const translations: Record<Language, TranslationDict> = {
  en,
  pl,
  es,
  de,
  fr,
  pt,
  it,
  uk,
  zh,
  ja,
  ko,
  hi,
};

export interface LanguageOption {
  code: Language;
  label: string;
}

/** Languages shown in the selector, self-named (native form). */
export const LANGUAGES: LanguageOption[] = [
  { code: 'en', label: 'English' },
  { code: 'pl', label: 'Polski' },
  { code: 'es', label: 'Español' },
  { code: 'de', label: 'Deutsch' },
  { code: 'fr', label: 'Français' },
  { code: 'pt', label: 'Português' },
  { code: 'it', label: 'Italiano' },
  { code: 'uk', label: 'Українська' },
  { code: 'zh', label: '中文' },
  { code: 'ja', label: '日本語' },
  { code: 'ko', label: '한국어' },
  { code: 'hi', label: 'हिन्दी' },
];

export const DEFAULT_LANGUAGE: Language = 'en';
