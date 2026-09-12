/**
 * F6-11: link detection in chat text. A relative/`.md` link (or a bare
 * relative-path mention) opens the Preview panel instead of navigating the
 * browser away; a normal `http(s)://` link is left completely alone (still
 * `target="_blank"`, no interception).
 */

import { Component, provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { SessionMeta } from '../../../core/engine.dtos';
import { ExplorerSelectionStore } from '../../../ui/right-drawer/panels/explorer-selection.store';
import { ShellStore } from '../../../ui/shell/shell.store';
import { ChatSessionStore } from '../chat-session.store';
import { TextPartComponent } from './text-part';

const META: SessionMeta = {
  id: 's1',
  directory: '/proj',
  agent: 'code',
  created_at: 0,
  updated_at: 0,
  usage: { input_tokens: 0, output_tokens: 0 },
} as SessionMeta;

@Component({
  imports: [TextPartComponent],
  template: `<app-text-part [part]="part" />`,
})
class HostComponent {
  part = { type: 'text' as const, text: '' };
}

describe('TextPartComponent link detection (F6-11)', () => {
  let fixture: ComponentFixture<HostComponent>;
  let selection: ExplorerSelectionStore;
  let shell: ShellStore;

  beforeEach(() => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [HostComponent],
      providers: [provideZonelessChangeDetection(), provideRouter([])],
    });
    TestBed.inject(ChatSessionStore).meta.set(META);
    selection = TestBed.inject(ExplorerSelectionStore);
    shell = TestBed.inject(ShellStore);
  });

  function render(text: string): HTMLElement {
    fixture = TestBed.createComponent(HostComponent);
    fixture.componentInstance.part = { type: 'text', text };
    fixture.detectChanges();
    return fixture.nativeElement;
  }

  it('opens Preview for a relative markdown link', () => {
    const el = render('See [the plan](./analysis/plan.md) for details.');
    const anchor = el.querySelector('a[data-preview-href]') as HTMLAnchorElement;
    expect(anchor).toBeTruthy();

    anchor.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));

    expect(selection.directory()).toBe('/proj');
    expect(selection.selectedPath()).toBe('analysis/plan.md');
    expect(shell.rightDrawerPanels().preview).toBeTrue();
  });

  it('opens Preview for a bare relative .md mention', () => {
    const el = render('Check docs/readme.md before shipping.');
    const anchor = el.querySelector('a[data-preview-href]') as HTMLAnchorElement;
    expect(anchor?.textContent).toBe('docs/readme.md');

    anchor.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    expect(selection.selectedPath()).toBe('docs/readme.md');
  });

  it('leaves a regular http(s) link unaffected', () => {
    const el = render('Full docs at [here](https://example.com/readme).');
    const anchor = el.querySelector('a') as HTMLAnchorElement;
    expect(anchor.getAttribute('target')).toBe('_blank');
    expect(anchor.hasAttribute('data-preview-href')).toBeFalse();

    const event = new MouseEvent('click', { bubbles: true, cancelable: true });
    anchor.dispatchEvent(event);

    expect(event.defaultPrevented).toBeFalse();
    expect(selection.selectedPath()).toBeNull();
    expect(shell.rightDrawerPanels().preview).toBeFalse();
  });

  it('keeps the sandboxed HTML-preview right-click feature working', () => {
    const el = render('```html\n<b>hi</b>\n```');
    const code = el.querySelector('pre code') as HTMLElement;
    const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true });
    code.dispatchEvent(event);
    fixture.detectChanges();

    expect(event.defaultPrevented).toBeTrue();
    const frame = el.querySelector('app-html-preview');
    expect(frame).toBeTruthy();
  });
});
