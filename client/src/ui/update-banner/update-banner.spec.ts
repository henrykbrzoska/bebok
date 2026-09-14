/**
 * `UpdateBanner`: hidden until the store offers a version, "Install" for an
 * installable update (disabled while an agent turn runs), "Download" for a
 * browser-mode one, "Later" dismisses, and the ready phase offers a restart.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { AvailableUpdate, UpdatePhase, UpdateStore } from '../../core/update.store';
import { UpdateBanner } from './update-banner';

describe('UpdateBanner', () => {
  let fixture: ComponentFixture<UpdateBanner>;
  let available: ReturnType<typeof signal<AvailableUpdate | null>>;
  let phase: ReturnType<typeof signal<UpdatePhase>>;
  let installBlocked: ReturnType<typeof signal<boolean>>;
  let dismissed: ReturnType<typeof signal<string | null>>;
  let install: jasmine.Spy;
  let relaunch: jasmine.Spy;
  let openReleasePage: jasmine.Spy;

  function update(installable: boolean): AvailableUpdate {
    return {
      version: '1.6.1',
      currentVersion: '1.6.0',
      notes: null,
      date: null,
      installable,
      releaseUrl: 'https://github.com/henrykbrzoska/bebok/releases/tag/1.6.1',
    };
  }

  beforeEach(() => {
    available = signal<AvailableUpdate | null>(null);
    phase = signal<UpdatePhase>('idle');
    installBlocked = signal(false);
    dismissed = signal<string | null>(null);
    install = jasmine.createSpy('install');
    relaunch = jasmine.createSpy('relaunch');
    openReleasePage = jasmine.createSpy('openReleasePage');
    const bannerVisible = () =>
      phase() === 'ready' || (available() !== null && available()!.version !== dismissed());
    TestBed.configureTestingModule({
      imports: [UpdateBanner],
      providers: [
        provideZonelessChangeDetection(),
        {
          provide: UpdateStore,
          useValue: {
            available,
            phase,
            installBlocked,
            bannerVisible,
            error: signal(null),
            feedMissing: signal(false),
            progressPercent: signal(null),
            install,
            relaunch,
            openReleasePage,
            dismiss: () => dismissed.set(available()?.version ?? null),
          },
        },
      ],
    });
    fixture = TestBed.createComponent(UpdateBanner);
    fixture.detectChanges();
  });

  function query(testId: string): HTMLElement | null {
    return (fixture.nativeElement as HTMLElement).querySelector(`[data-testid="${testId}"]`);
  }

  it('stays hidden without an update', () => {
    expect(query('update-banner')).toBeNull();
  });

  it('offers install for an installable update and calls the store', async () => {
    available.set(update(true));
    await fixture.whenStable();
    const button = query('update-install') as HTMLButtonElement;
    expect(button).not.toBeNull();
    expect(query('update-download')).toBeNull();
    button.click();
    expect(install).toHaveBeenCalled();
  });

  it('disables install and explains while an agent turn is running', async () => {
    available.set(update(true));
    installBlocked.set(true);
    await fixture.whenStable();
    expect((query('update-install') as HTMLButtonElement).disabled).toBeTrue();
    expect(query('update-blocked')).not.toBeNull();
  });

  it('offers a download link for a non-installable update', async () => {
    available.set(update(false));
    await fixture.whenStable();
    expect(query('update-install')).toBeNull();
    (query('update-download') as HTMLButtonElement).click();
    expect(openReleasePage).toHaveBeenCalled();
  });

  it('hides after "Later"', async () => {
    available.set(update(true));
    await fixture.whenStable();
    (query('update-later') as HTMLButtonElement).click();
    await fixture.whenStable();
    expect(query('update-banner')).toBeNull();
  });

  it('offers a restart once the update is installed', async () => {
    available.set(update(true));
    phase.set('ready');
    await fixture.whenStable();
    expect(query('update-install')).toBeNull();
    (query('update-restart') as HTMLButtonElement).click();
    expect(relaunch).toHaveBeenCalled();
  });
});
