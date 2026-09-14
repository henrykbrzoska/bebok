import { provideZonelessChangeDetection, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { Router } from '@angular/router';

import { EngineClient } from '../engine-client.service';
import { EngineTargetStore, TARGETS_KEY } from '../engine-target.store';
import { ShareStore } from './share.store';

describe('ShareStore (1.8)', () => {
  const sid = '11111111-2222-3333-4444-555555555555';
  const link = `bebok://share?v=1&ep=http%3A%2F%2F100.64.0.7%3A8790&s=${sid}&t=${'t'.repeat(40)}&n=rafal-pc`;
  let switchTarget: jasmine.Spy;
  let navigate: jasmine.Spy;
  let store: ShareStore;
  let targets: EngineTargetStore;

  beforeEach(() => {
    localStorage.clear();
    switchTarget = jasmine
      .createSpy('switchTarget')
      .and.callFake(async (id: string) => targets.byId(id));
    navigate = jasmine.createSpy('navigate').and.resolveTo(true);
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        { provide: EngineClient, useValue: { switchTarget, isTauri: signal(false) } },
        { provide: Router, useValue: { navigate } },
      ],
    });
    targets = TestBed.inject(EngineTargetStore);
    store = TestBed.inject(ShareStore);
  });

  it('joins a pasted link: persisted share target, switch, navigate to the chat', async () => {
    const target = await store.join(link);
    expect(target).not.toBeNull();
    expect(target!.kind).toBe('share');
    expect(target!.sessionId).toBe(sid);
    expect(target!.endpoints).toEqual(['http://100.64.0.7:8790']);
    expect(switchTarget).toHaveBeenCalledWith(`share:${sid}`);
    expect(navigate).toHaveBeenCalledWith(['/chat', sid]);
    expect(store.shares().length).toBe(1);
    const stored = JSON.parse(localStorage.getItem(TARGETS_KEY)!) as Array<Record<string, unknown>>;
    expect(stored[0]['kind']).toBe('share');
    expect(stored[0]['token']).toBeUndefined();
    expect(store.error()).toBeNull();
  });

  it('reports a bad link without registering anything', async () => {
    expect(await store.join('https://nope')).toBeNull();
    expect(store.error()).toContain('bebok://share');
    expect(store.shares().length).toBe(0);
    expect(switchTarget).not.toHaveBeenCalled();
  });
});
