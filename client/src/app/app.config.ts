import { ApplicationConfig, provideBrowserGlobalErrorListeners, provideZonelessChangeDetection } from '@angular/core';
import { provideRouter } from '@angular/router';

import { ENGINE_API } from '../core/engine-api';
import { EngineClient } from '../core/engine-client.service';
import { routes } from './app.routes';

export const appConfig: ApplicationConfig = {
  providers: [
    provideBrowserGlobalErrorListeners(),
    // Zoneless (SPEC §7 / M3): no zone.js runtime; signals drive change
    // detection. Only real streams (PTY bytes later, M5) use RxJS.
    provideZonelessChangeDetection(),
    provideRouter(routes),
    // WP-M2 (F10-6): the engine contract resolves to the HTTP client. A
    // relay-backed implementation (1.7) swaps this one line.
    { provide: ENGINE_API, useExisting: EngineClient },
  ],
};
