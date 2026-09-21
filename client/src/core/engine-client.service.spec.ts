/**
 * Phase 2: EngineClient disconnect / reconnect + profile delegation.
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { EngineClient } from './engine-client.service';
import { EventsStore } from './events.store';
import { setEngineToken } from './auth.interceptor';

describe('EngineClient (Phase 2: disconnect / profile)', () => {
  let engine: EngineClient;

  beforeEach(() => {
    localStorage.clear();
    setEngineToken(null);
    TestBed.configureTestingModule({
      providers: [EngineClient, EventsStore],
    });
    engine = TestBed.inject(EngineClient);
  });

  afterEach(() => {
    localStorage.clear();
    setEngineToken(null);
  });

  it('disconnect() clears connection, token, unauthorized, and sets isDisconnected', () => {
    // Simulate a connected state.
    engine.connection.set({ kind: 'http', baseUrl: 'http://127.0.0.1:8787' });
    engine.unauthorized.set(true);
    engine.isDisconnected.set(false);

    engine.disconnect();

    expect(engine.connection()).toBeNull();
    expect(engine.unauthorized()).toBeFalse();
    expect(engine.isDisconnected()).toBeTrue();
  });

  it('connect() throws when isDisconnected is true and no connection exists', async () => {
    engine.disconnect();
    await expectAsync(engine.connect()).toBeRejectedWithError('engine disconnected by user');
  });

  it('connect() returns existing connection even when isDisconnected is true', async () => {
    const conn = { kind: 'http' as const, baseUrl: 'http://127.0.0.1:8787' };
    engine.connection.set(conn);
    engine.disconnect();
    // Should return the cached connection, not throw.
    const result = await engine.connect();
    expect(result).toBe(conn);
  });

  it('reconnect() clears isDisconnected so automatic reconnect works again', async () => {
    engine.disconnect();
    expect(engine.isDisconnected()).toBeTrue();

    // Mock connect to succeed via the real transport (will try the default URL).
    // We test the flag clearing, not the actual connection.
    try {
      await engine.reconnect('http://127.0.0.1:8787');
    } catch {
      // Expected: ping may fail if no engine is running, but the flag is cleared.
    }
    expect(engine.isDisconnected()).toBeFalse();
  });

  it('getProfile / saveFixedProfile / clearProfile delegate to transport', () => {
    expect(engine.getProfile().kind).toBe('manual');
    engine.saveFixedProfile('http://127.0.0.1:9999', 'tok-123');
    expect(engine.isFixedProfile()).toBeTrue();
    expect(engine.getProfile().baseUrl).toBe('http://127.0.0.1:9999');
    expect(engine.getProfile().token).toBe('tok-123');
    engine.clearProfile();
    expect(engine.isFixedProfile()).toBeFalse();
  });
});
