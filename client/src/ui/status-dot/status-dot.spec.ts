/**
 * F9-1: the hidden-label variant of the status dot must not leak out of its
 * row. The label is visually hidden with `position: absolute`; if the host
 * were not positioned, the label's containing block would be the document and
 * every dot in a scrolled-out sidebar row would extend the page's scrollable
 * overflow (the "second scrollbar / topbar scrolls away" bug).
 */

import { Component, provideZonelessChangeDetection } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { StatusDot } from './status-dot';

@Component({
  imports: [StatusDot],
  template: `
    <div class="clip" style="height: 40px; overflow: auto; position: static">
      @for (i of rows; track i) {
        <div style="height: 30px">
          <app-status-dot label="idle" tone="idle" [labelHidden]="true" />
        </div>
      }
    </div>
  `,
})
class HostCmp {
  readonly rows = Array.from({ length: 200 }, (_, i) => i);
}

describe('StatusDot (F9-1 hidden label containment)', () => {
  beforeEach(() => {
    TestBed.configureTestingModule({
      imports: [HostCmp],
      providers: [provideZonelessChangeDetection()],
    });
  });

  it('anchors the visually-hidden label to the dot, not to the document', () => {
    const fixture = TestBed.createComponent(HostCmp);
    fixture.detectChanges();
    const host = fixture.nativeElement as HTMLElement;
    const dot = host.querySelector('app-status-dot') as HTMLElement;
    expect(getComputedStyle(dot).position).toBe('relative');
    const label = host.querySelector('app-status-dot .label') as HTMLElement;
    // offsetParent is the nearest positioned ancestor: the dot itself, never
    // <body> (which would mean the label is laid out against the document).
    expect(label.offsetParent).toBe(dot);
  });

  it('does not grow the document when 200 rows are scrolled out of a 40px list', () => {
    const before = document.documentElement.scrollHeight;
    const fixture = TestBed.createComponent(HostCmp);
    fixture.detectChanges();
    const host = fixture.nativeElement as HTMLElement;
    const labels = host.querySelectorAll('app-status-dot .label');
    expect(labels.length).toBe(200);
    // 200 rows x 30px live inside the 40px scroll box. Were the hidden labels
    // laid out against the document, the page would grow by ~6000px.
    const grown = document.documentElement.scrollHeight - before;
    expect(grown).toBeLessThan(200);
  });
});
