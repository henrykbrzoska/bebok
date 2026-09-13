/**
 * WP-M5 / F10-17: `SpeechService` - transcript on success, `null` (never a
 * throw) on denied permission / recogniser error / nothing recognised, and
 * "unsupported" when the device has no recognition service or the shell is
 * not Capacitor.
 */

import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';

import { SpeechBridge, SpeechService } from './speech.service';

function bridge(overrides: Partial<SpeechBridge> = {}): SpeechBridge {
  return {
    available: async () => ({ available: true }),
    requestPermissions: async () => ({ speechRecognition: 'granted' }),
    start: async () => ({ matches: ['hello world'] }),
    stop: async () => undefined,
    ...overrides,
  };
}

describe('SpeechService (F10-17)', () => {
  let service: SpeechService;

  beforeEach(() => {
    TestBed.configureTestingModule({ providers: [provideZonelessChangeDetection()] });
    service = TestBed.inject(SpeechService);
  });

  it('is unsupported outside Capacitor without probing the plugin', async () => {
    expect(service.supported()).toBeFalse();
    let probed = false;
    service.configure({
      bridge: bridge({
        available: async () => {
          probed = true;
          return { available: true };
        },
      }),
    });
    expect(await service.probe()).toBeFalse();
    expect(await service.listen('en-US')).toBeNull();
    expect(probed).toBeFalse();
  });

  it('probes once and returns the transcript', async () => {
    let probes = 0;
    const seen: unknown[] = [];
    service.configure({
      native: true,
      bridge: bridge({
        available: async () => {
          probes++;
          return { available: true };
        },
        start: async (options) => {
          seen.push(options);
          return { matches: ['  fix the tests  ', 'six the tests'] };
        },
      }),
    });
    expect(service.supported()).toBeNull();
    expect(await service.probe()).toBeTrue();
    expect(await service.probe()).toBeTrue();
    expect(probes).toBe(1);
    expect(service.supported()).toBeTrue();

    expect(await service.listen('pl-PL')).toBe('fix the tests');
    expect(seen[0]).toEqual({ language: 'pl-PL', maxResults: 1, partialResults: false, popup: false });
    expect(service.listening()).toBeFalse();
    expect(service.error()).toBeNull();
  });

  it('hides itself when no recognition service exists', async () => {
    service.configure({ native: true, bridge: bridge({ available: async () => ({ available: false }) }) });
    expect(await service.probe()).toBeFalse();
    expect(service.supported()).toBeFalse();
    expect(await service.listen()).toBeNull();
  });

  it('reports a denied permission and recogniser errors as null + error', async () => {
    service.configure({
      native: true,
      bridge: bridge({ requestPermissions: async () => ({ speechRecognition: 'denied' }) }),
    });
    expect(await service.listen()).toBeNull();
    expect(service.error()).toBe('permission');

    service.configure({
      native: true,
      bridge: bridge({
        start: async () => {
          throw new Error('No match');
        },
      }),
    });
    expect(await service.listen()).toBeNull();
    expect(service.error()).toBe('No match');
    expect(service.listening()).toBeFalse();

    service.configure({ native: true, bridge: bridge({ start: async () => ({ matches: [] }) }) });
    expect(await service.listen()).toBeNull();
  });

  it('stop() ends an in-progress dictation', async () => {
    let stopped = false;
    let resolveStart: (v: { matches?: string[] }) => void = () => undefined;
    service.configure({
      native: true,
      bridge: bridge({
        start: () => new Promise((resolve) => (resolveStart = resolve)),
        stop: async () => {
          stopped = true;
          resolveStart({ matches: [] });
        },
      }),
    });
    const pending = service.listen();
    for (let i = 0; i < 20 && !service.listening(); i++) {
      await Promise.resolve();
    }
    expect(service.listening()).toBeTrue();
    await service.stop();
    expect(stopped).toBeTrue();
    expect(await pending).toBeNull();
    expect(service.listening()).toBeFalse();
  });
});
