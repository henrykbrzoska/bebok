/**
 * WP-M5 / F10-16: onboarding - the three branches (chat locally, pair with
 * desktop, manual address) against a fake `ENGINE_API` / real
 * `EngineTargetStore`, plus the embedded-failure (`platformError`) screen
 * and its retry.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';

import { ENGINE_API } from '../../../core/engine-api';
import { EngineTargetStore, TARGETS_KEY } from '../../../core/engine-target.store';
import {
  ONBOARDED_KEY,
  OnboardingView,
  normalizeEngineAddress,
  readOnboarded,
  remoteUrlTargetId,
  writeOnboarded,
} from './onboarding';

interface EngineFake {
  connection: ReturnType<typeof signal<{ kind: 'http'; baseUrl: string } | null>>;
  connected: ReturnType<typeof signal<boolean>>;
  unauthorized: ReturnType<typeof signal<boolean>>;
  connect: jasmine.Spy;
  switchTarget: jasmine.Spy;
  ping: jasmine.Spy;
}

describe('OnboardingView (F10-16)', () => {
  let fixture: ComponentFixture<OnboardingView>;
  let engine: EngineFake;
  let targets: EngineTargetStore;
  let navigate: jasmine.Spy;
  let done: number;

  function el<T extends HTMLElement>(testId: string): T | null {
    return (fixture.nativeElement as HTMLElement).querySelector<T>(`[data-testid="${testId}"]`);
  }

  async function settle(): Promise<void> {
    for (let i = 0; i < 6; i++) {
      await Promise.resolve();
      await fixture.whenStable();
    }
    fixture.detectChanges();
  }

  beforeEach(async () => {
    localStorage.clear();
    const connection = signal<{ kind: 'http'; baseUrl: string } | null>(null);
    engine = {
      connection,
      connected: signal(false),
      unauthorized: signal(false),
      connect: jasmine.createSpy('connect').and.callFake(async () => {
        connection.set({ kind: 'http', baseUrl: 'http://127.0.0.1:1' });
        engine.connected.set(true);
        targets.upsert({
          id: 'embedded',
          kind: 'embedded',
          label: 'This device',
          baseUrl: 'http://127.0.0.1:1',
          token: 't',
          ephemeral: true,
        });
        targets.setActive('embedded');
        targets.platformError.set(null);
        return connection();
      }),
      switchTarget: jasmine.createSpy('switchTarget').and.callFake(async (id: string) => {
        const target = targets.byId(id)!;
        targets.setActive(id);
        connection.set({ kind: 'http', baseUrl: target.baseUrl });
        engine.connected.set(true);
        return target;
      }),
      ping: jasmine.createSpy('ping').and.resolveTo(undefined),
    };
    TestBed.configureTestingModule({
      imports: [OnboardingView],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        { provide: ENGINE_API, useValue: engine },
      ],
    });
    targets = TestBed.inject(EngineTargetStore);
    await targets.ready;
    navigate = spyOn(TestBed.inject(Router), 'navigate').and.resolveTo(true);
    done = 0;
    fixture = TestBed.createComponent(OnboardingView);
    fixture.componentInstance.done.subscribe(() => done++);
    fixture.detectChanges();
  });

  afterEach(() => {
    fixture.destroy();
    localStorage.clear();
  });

  it('"Chat locally" connects the platform engine and completes', async () => {
    expect(el('onboarding')).not.toBeNull();
    el<HTMLButtonElement>('onboarding-local')!.click();
    await settle();
    expect(engine.connect).toHaveBeenCalledTimes(1);
    expect(targets.activeId()).toBe('embedded');
    expect(done).toBe(1);
    expect(el('onboarding-error')).toBeNull();
  });

  it('"Chat locally" completes immediately when the engine is already up', async () => {
    engine.connected.set(true);
    fixture.detectChanges();
    expect(el('onboarding-local')!.textContent).toContain('Engine running');
    el<HTMLButtonElement>('onboarding-local')!.click();
    await settle();
    expect(engine.connect).not.toHaveBeenCalled();
    expect(done).toBe(1);
  });

  it('shows the embedded failure and retries through connect()', async () => {
    targets.platformError.set('embedded engine unavailable: no bundled ABI');
    fixture.detectChanges();
    expect(el('onboarding-platform-error')!.textContent).toContain('no bundled ABI');
    expect(el('onboarding-local')!.textContent).toContain('Retry');

    // First retry keeps failing: the error stays, nothing completes.
    engine.connect.and.callFake(async () => {
      targets.platformError.set('embedded engine unavailable: still broken');
      throw new Error('embedded engine unavailable: still broken');
    });
    el<HTMLButtonElement>('onboarding-local')!.click();
    await settle();
    expect(done).toBe(0);
    expect(el('onboarding-error')!.textContent).toContain('still broken');

    // Second retry succeeds.
    engine.connect.and.callFake(async () => {
      targets.platformError.set(null);
      engine.connected.set(true);
      return { kind: 'http', baseUrl: 'http://127.0.0.1:1' };
    });
    el<HTMLButtonElement>('onboarding-local')!.click();
    await settle();
    expect(done).toBe(1);
    expect(el('onboarding-platform-error')).toBeNull();
  });

  it('offers to continue with the fallback engine when the embedded one failed', async () => {
    targets.platformError.set('embedded engine unavailable: x');
    fixture.detectChanges();
    expect(el('onboarding-continue')).toBeNull();
    engine.connected.set(true); // connect() fell back to a paired desktop
    fixture.detectChanges();
    el<HTMLButtonElement>('onboarding-continue')!.click();
    await settle();
    expect(targets.platformError()).toBeNull();
    expect(done).toBe(1);
    expect(engine.connect).not.toHaveBeenCalled();
  });

  it('"Pair with desktop" hands over to the Remote tab', async () => {
    el<HTMLButtonElement>('onboarding-pair')!.click();
    await settle();
    expect(navigate).toHaveBeenCalledWith(['/m/remote'], { queryParams: { pair: 1 } });
    expect(done).toBe(1);
  });

  it('manual address becomes a persisted remote-url target and is pinged', async () => {
    el<HTMLButtonElement>('onboarding-address-toggle')!.click();
    await settle();
    const input = el<HTMLInputElement>('onboarding-address')!;
    input.value = '192.168.1.10:8790/?token=abc';
    input.dispatchEvent(new Event('input'));
    await settle();
    el<HTMLButtonElement>('onboarding-address-connect')!.click();
    await settle();

    const id = 'remote-url:192.168.1.10:8790';
    expect(engine.switchTarget).toHaveBeenCalledWith(id);
    expect(engine.ping).toHaveBeenCalled();
    expect(targets.byId(id)).toEqual(
      jasmine.objectContaining({ kind: 'remote-url', baseUrl: 'http://192.168.1.10:8790', token: 'abc' }),
    );
    expect(targets.activeId()).toBe(id);
    const stored = JSON.parse(localStorage.getItem(TARGETS_KEY)!) as Array<{ id: string }>;
    expect(stored.map((t) => t.id)).toEqual([id]);
    expect(localStorage.getItem(TARGETS_KEY)).not.toContain('abc');
    expect(done).toBe(1);
  });

  it('a failed ping removes the target and shows the error', async () => {
    engine.ping.and.rejectWith(new Error('Failed to fetch'));
    fixture.componentInstance.addressOpen.set(true);
    fixture.componentInstance.address.set('http://10.0.0.5:8790');
    await fixture.componentInstance.useAddress();
    await settle();
    expect(targets.byId('remote-url:10.0.0.5:8790')).toBeNull();
    expect(el('onboarding-error')!.textContent).toContain('Failed to fetch');
    expect(done).toBe(0);
  });

  it('helpers: address normalisation, target ids, onboarded flag', () => {
    expect(normalizeEngineAddress('  192.168.1.10:8790/ ')).toBe('http://192.168.1.10:8790');
    expect(normalizeEngineAddress('https://host.tld/')).toBe('https://host.tld');
    expect(normalizeEngineAddress('ftp://x')).toBe('');
    expect(normalizeEngineAddress('')).toBe('');
    expect(remoteUrlTargetId('http://a:1')).toBe('remote-url:a:1');
    expect(readOnboarded()).toBeFalse();
    writeOnboarded(true);
    expect(localStorage.getItem(ONBOARDED_KEY)).toBe('1');
    expect(readOnboarded()).toBeTrue();
    writeOnboarded(false);
    expect(readOnboarded()).toBeFalse();
  });
});
