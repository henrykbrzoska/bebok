/**
 * `UpdateStore`: version comparison, the GitHub-API detection path used in
 * browser mode, dismissal persistence and the "agent turn running" install
 * guard. The desktop (`invoke`) path is exercised end-to-end against a real
 * release, not here.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { EngineClient } from './engine-client.service';
import { SessionActivityStore } from './session-activity.store';
import { UpdateStore, compareVersions } from './update.store';

describe('compareVersions', () => {
  it('orders numeric components and ignores a leading v', () => {
    expect(compareVersions('1.6.0', '1.6.1')).toBeLessThan(0);
    expect(compareVersions('v1.10.0', '1.9.9')).toBeGreaterThan(0);
    expect(compareVersions('1.6', '1.6.0')).toBe(0);
  });

  it('sorts a pre-release before its release', () => {
    expect(compareVersions('1.7.0-rc.1', '1.7.0')).toBeLessThan(0);
    expect(compareVersions('1.7.0', '1.7.0-rc.1')).toBeGreaterThan(0);
    expect(compareVersions('1.7.0-rc.1', '1.7.0-rc.2')).toBeLessThan(0);
  });
});

describe('UpdateStore', () => {
  let store: UpdateStore;
  let running: ReturnType<typeof signal<ReadonlySet<string>>>;
  let fetchSpy: jasmine.Spy;

  function release(tag: string, extra: Record<string, unknown> = {}): Response {
    return new Response(
      JSON.stringify({
        tag_name: tag,
        html_url: `https://github.com/henrykbrzoska/bebok/releases/tag/${tag}`,
        body: 'notes',
        published_at: '2026-09-13T15:14:36Z',
        draft: false,
        prerelease: false,
        ...extra,
      }),
      { status: 200, headers: { 'Content-Type': 'application/json' } },
    );
  }

  beforeEach(() => {
    localStorage.removeItem('bebok.update.dismissed');
    running = signal<ReadonlySet<string>>(new Set());
    fetchSpy = spyOn(window, 'fetch');
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        {
          provide: EngineClient,
          useValue: {
            isTauri: signal(false),
            connect: async () => ({ kind: 'http', baseUrl: 'http://127.0.0.1:8787' }),
            getVersion: async () => ({ version: '1.6.0' }),
          },
        },
        { provide: SessionActivityStore, useValue: { runningSessions: running } },
      ],
    });
    store = TestBed.inject(UpdateStore);
  });

  afterEach(() => {
    store.stop();
    localStorage.removeItem('bebok.update.dismissed');
  });

  it('reads the engine version and offers a newer GitHub release as a download', async () => {
    fetchSpy.and.resolveTo(release('1.6.1'));
    await store.check();
    expect(store.engineVersion()).toBe('1.6.0');
    expect(store.phase()).toBe('idle');
    expect(store.available()).toEqual(
      jasmine.objectContaining({ version: '1.6.1', currentVersion: '1.6.0', installable: false }),
    );
    expect(store.bannerVisible()).toBeTrue();
  });

  it('reports up to date when the latest tag is not newer', async () => {
    fetchSpy.and.resolveTo(release('v1.6.0'));
    await store.check();
    expect(store.available()).toBeNull();
    expect(store.bannerVisible()).toBeFalse();
    expect(store.lastCheckedAt()).not.toBeNull();
  });

  it('ignores pre-releases', async () => {
    fetchSpy.and.resolveTo(release('1.7.0-rc.1', { prerelease: true }));
    await store.check();
    expect(store.available()).toBeNull();
  });

  it('surfaces a failed feed as an error phase', async () => {
    fetchSpy.and.resolveTo(new Response('', { status: 503 }));
    await store.check();
    expect(store.phase()).toBe('error');
    expect(store.error()).toContain('503');
  });

  it('hides the banner for a dismissed version only', async () => {
    fetchSpy.and.resolveTo(release('1.6.1'));
    await store.check();
    store.dismiss();
    expect(store.bannerVisible()).toBeFalse();
    expect(localStorage.getItem('bebok.update.dismissed')).toBe('1.6.1');

    fetchSpy.and.resolveTo(release('1.6.2'));
    await store.check();
    expect(store.bannerVisible()).toBeTrue();
  });

  it('lists recent releases with the running one marked', async () => {
    const list = [
      {
        tag_name: '1.6.1',
        html_url: 'u1',
        body: null,
        published_at: null,
        draft: false,
        prerelease: false,
      },
      {
        tag_name: '1.6.0',
        html_url: 'u0',
        body: null,
        published_at: null,
        draft: false,
        prerelease: false,
      },
      {
        tag_name: '1.7.0-1',
        html_url: 'ud',
        body: null,
        published_at: null,
        draft: true,
        prerelease: true,
      },
    ];
    fetchSpy.and.callFake((input: RequestInfo | URL) =>
      Promise.resolve(
        String(input).includes('/releases/latest')
          ? release('1.6.1')
          : new Response(JSON.stringify(list), {
              status: 200,
              headers: { 'Content-Type': 'application/json' },
            }),
      ),
    );
    await store.check();
    await store.loadReleases();
    const releases = store.releases();
    expect(releases?.map((entry) => entry.version)).toEqual(['1.6.1', '1.6.0']);
    expect(releases?.[1].current).toBeTrue();
  });

  it('blocks installing while an agent turn is running', () => {
    expect(store.installBlocked()).toBeFalse();
    running.set(new Set(['s1']));
    expect(store.installBlocked()).toBeTrue();
  });
});
