/**
 * `UpdatesView`: shows the running versions, triggers a check, offers the
 * install / download action and renders the release list with the running
 * release marked.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { AvailableUpdate, ReleaseSummary, UpdatePhase, UpdateStore } from '../../core/update.store';
import { UpdatesView } from './updates';

describe('UpdatesView', () => {
  let fixture: ComponentFixture<UpdatesView>;
  let available: ReturnType<typeof signal<AvailableUpdate | null>>;
  let releases: ReturnType<typeof signal<ReleaseSummary[] | null>>;
  let phase: ReturnType<typeof signal<UpdatePhase>>;
  let check: jasmine.Spy;
  let loadReleases: jasmine.Spy;

  beforeEach(() => {
    available = signal<AvailableUpdate | null>(null);
    releases = signal<ReleaseSummary[] | null>(null);
    phase = signal<UpdatePhase>('idle');
    check = jasmine.createSpy('check');
    loadReleases = jasmine.createSpy('loadReleases').and.resolveTo();
    TestBed.configureTestingModule({
      imports: [UpdatesView],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        {
          provide: UpdateStore,
          useValue: {
            shellVersion: signal('1.6.0'),
            engineVersion: signal('1.6.0'),
            versionMismatch: signal(false),
            debugBuild: signal(false),
            available,
            releases,
            releasesLoading: signal(false),
            releasesError: signal(null),
            phase,
            error: signal(null),
            lastCheckedAt: signal(null),
            installBlocked: signal(false),
            check,
            loadReleases,
            install: jasmine.createSpy('install'),
            openReleasePage: jasmine.createSpy('openReleasePage'),
          },
        },
      ],
    });
    fixture = TestBed.createComponent(UpdatesView);
    fixture.detectChanges();
  });

  function query(selector: string): HTMLElement | null {
    return (fixture.nativeElement as HTMLElement).querySelector(selector);
  }

  it('loads the release list on open and shows both versions', () => {
    expect(loadReleases).toHaveBeenCalled();
    expect(query('[data-testid="version-card"]')?.textContent).toContain('1.6.0');
  });

  it('runs a check from the button', () => {
    (query('[data-testid="check-updates"]') as HTMLButtonElement).click();
    expect(check).toHaveBeenCalled();
  });

  it('marks the installed release in the list', async () => {
    releases.set([
      { version: '1.6.1', url: 'u1', notes: null, date: null, prerelease: false, current: false },
      { version: '1.6.0', url: 'u0', notes: null, date: null, prerelease: false, current: true },
    ]);
    await fixture.whenStable();
    const rows = (fixture.nativeElement as HTMLElement).querySelectorAll(
      '[data-testid="release-list"] .release',
    );
    expect(rows.length).toBe(2);
    expect(rows[1].classList).toContain('current');
    expect(rows[0].classList).not.toContain('current');
  });
});
