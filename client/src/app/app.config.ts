import {
  ApplicationConfig,
  provideAppInitializer,
  provideBrowserGlobalErrorListeners,
  provideZonelessChangeDetection,
} from '@angular/core';
import { provideRouter } from '@angular/router';

import { routes } from './app.routes';
import { installTextUndo } from '../core/text-undo';

export const appConfig: ApplicationConfig = {
  providers: [
    provideBrowserGlobalErrorListeners(),
    // Zoneless (SPEC §7 / M3): no zone.js runtime; signals drive change
    // detection. Only real streams (PTY bytes later, M5) use RxJS.
    provideZonelessChangeDetection(),
    provideRouter(routes),
    // Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z in every text field (native stack is unreliable).
    provideAppInitializer(() => {
      installTextUndo();
    }),
  ],
};
