/**
 * F6-10: `app-markdown-view` intercepts a relative link's click and emits the
 * resolved engine path instead of letting the browser navigate; an absolute
 * link is left alone (no interception, no `(linkClick)`).
 */

import { Component, provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { MarkdownViewComponent } from './markdown-view';

@Component({
  imports: [MarkdownViewComponent],
  template: `<app-markdown-view
    [source]="source"
    [basePath]="basePath"
    (linkClick)="onLinkClick($event)"
  />`,
})
class HostComponent {
  source = '[see](./other.md)\n\n[ext](https://example.com)';
  basePath = 'docs/readme.md';
  lastClick: string | null = null;
  onLinkClick(path: string): void {
    this.lastClick = path;
  }
}

describe('MarkdownViewComponent', () => {
  let fixture: ComponentFixture<HostComponent>;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [provideZonelessChangeDetection()],
    });
    fixture = TestBed.createComponent(HostComponent);
    fixture.detectChanges();
  });

  it('emits the resolved path when a relative link is clicked', () => {
    const anchor = fixture.nativeElement.querySelector(
      'a[data-relative]',
    ) as HTMLAnchorElement;
    expect(anchor).toBeTruthy();
    const event = new MouseEvent('click', { bubbles: true, cancelable: true });
    anchor.dispatchEvent(event);
    expect(fixture.componentInstance.lastClick).toBe('docs/other.md');
    expect(event.defaultPrevented).toBeTrue();
  });

  it('leaves an absolute link un-intercepted', () => {
    const anchors = fixture.nativeElement.querySelectorAll('a');
    const external = Array.from(anchors as NodeListOf<HTMLAnchorElement>).find(
      (a) => a.getAttribute('href') === 'https://example.com',
    );
    expect(external).toBeTruthy();
    expect(external?.hasAttribute('data-relative')).toBeFalse();
    const event = new MouseEvent('click', { bubbles: true, cancelable: true });
    external?.dispatchEvent(event);
    expect(fixture.componentInstance.lastClick).toBeNull();
    expect(event.defaultPrevented).toBeFalse();
  });
});
