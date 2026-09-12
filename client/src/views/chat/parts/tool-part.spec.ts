/**
 * F6-1: default expand state of a tool call.
 *
 * With the preference off, only the first call of a turn (`toolIndex` 0)
 * opens; with it on, every call opens. The header is a real `<button>` with
 * `aria-expanded`, so keyboard activation (Enter/Space) is native.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { Part } from '../../../core/engine.dtos';
import { UiPrefsStore } from '../../../core/ui-prefs.store';
import { ToolPartComponent } from './tool-part';

const READ: Part = {
  type: 'tool',
  id: 'tool-1',
  name: 'read',
  state: { state: 'completed', input: { path: 'src/main.ts' }, output: 'contents', title: 'read' },
};

describe('ToolPartComponent default state (F6-1)', () => {
  let fixture: ComponentFixture<ToolPartComponent>;
  let prefs: UiPrefsStore;

  function head(): HTMLButtonElement {
    return fixture.nativeElement.querySelector('.tool-head');
  }

  beforeEach(() => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [ToolPartComponent],
      providers: [provideZonelessChangeDetection(), provideRouter([])],
    });
    prefs = TestBed.inject(UiPrefsStore);
    prefs.setExpandToolCallsByDefault(false);
    fixture = TestBed.createComponent(ToolPartComponent);
    fixture.componentRef.setInput('part', READ);
  });

  afterEach(() => {
    prefs.setExpandToolCallsByDefault(false);
  });

  it('opens the first call of a turn and collapses later ones when the preference is off', async () => {
    fixture.componentRef.setInput('toolIndex', 0);
    await fixture.whenStable();
    expect(head().getAttribute('aria-expanded')).toBe('true');
    expect(fixture.nativeElement.querySelector('.tool-details')).not.toBeNull();

    fixture.componentRef.setInput('toolIndex', 3);
    await fixture.whenStable();
    expect(head().getAttribute('aria-expanded')).toBe('false');
    expect(head().classList.contains('collapsed')).toBeTrue();
    expect(fixture.nativeElement.querySelector('.tool-details')).toBeNull();
  });

  it('opens every call when the preference is on', async () => {
    prefs.setExpandToolCallsByDefault(true);
    fixture.componentRef.setInput('toolIndex', 5);
    await fixture.whenStable();
    expect(head().getAttribute('aria-expanded')).toBe('true');
  });

  it('toggles through the header button (native Enter/Space activation)', async () => {
    fixture.componentRef.setInput('toolIndex', 2);
    await fixture.whenStable();
    expect(head().tagName).toBe('BUTTON');
    expect(head().getAttribute('aria-expanded')).toBe('false');

    head().click();
    await fixture.whenStable();
    expect(head().getAttribute('aria-expanded')).toBe('true');
    expect(fixture.nativeElement.querySelector('.tool-details')).not.toBeNull();

    head().click();
    await fixture.whenStable();
    expect(head().getAttribute('aria-expanded')).toBe('false');
  });
});
