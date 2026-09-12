/**
 * F7-4: diff-view syntax highlighting. Add/remove line *backgrounds* still
 * come from `line.cls` (`.line.add`/`.line.del`, unchanged CSS) - only the
 * code inside each line is tokenized, and the leading `+`/`-`/` ` marker is
 * kept outside the highlighted span so it never gets a token color.
 */

import { Component, provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { preloadLanguage } from '../code-highlight/code-highlight';
import { DiffViewComponent } from './diff-view';
import { DiffViewStore } from './diff-view.store';

@Component({
  imports: [DiffViewComponent],
  template: `<app-diff-view [text]="text" [fileLabel]="fileLabel" />`,
})
class Host {
  text = '';
  fileLabel = '';
}

const TS_DIFF = [
  'diff --git a/src/a.ts b/src/a.ts',
  '--- a/src/a.ts',
  '+++ b/src/a.ts',
  '@@ -1,2 +1,2 @@',
  '-const old = 1;',
  '+const value: number = 2;',
  ' keep();',
].join('\n');

describe('DiffViewComponent syntax highlighting (F7-4)', () => {
  let fixture: ComponentFixture<Host>;

  beforeEach(async () => {
    localStorage.clear();
    await preloadLanguage('typescript');
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [Host],
      providers: [provideZonelessChangeDetection()],
    });
  });

  function render(text: string, fileLabel = ''): HTMLElement {
    fixture = TestBed.createComponent(Host);
    fixture.componentInstance.text = text;
    fixture.componentInstance.fileLabel = fileLabel;
    fixture.detectChanges();
    return fixture.nativeElement as HTMLElement;
  }

  it('keeps the +/- marker and add/del backgrounds while tokenizing the code', () => {
    const el = render(TS_DIFF, 'src/a.ts');
    const addLine = el.querySelector('.line.add code') as HTMLElement;
    const delLine = el.querySelector('.line.del code') as HTMLElement;

    expect(addLine.innerHTML.startsWith('+')).toBeTrue();
    expect(delLine.innerHTML.startsWith('-')).toBeTrue();
    // Tokenized: a `const` keyword span, not just escaped verbatim text.
    expect(addLine.innerHTML).toContain('hljs-keyword');
    expect(delLine.innerHTML).toContain('hljs-keyword');
    // The line's own text is still intact (marker + code), just wrapped.
    expect(addLine.textContent).toBe('+const value: number = 2;');
    expect(delLine.textContent).toBe('-const old = 1;');
  });

  it('falls back to the language named on the diff --git / +++ line when fileLabel is empty', () => {
    const el = render(TS_DIFF);
    const addLine = el.querySelector('.line.add code') as HTMLElement;
    expect(addLine.innerHTML).toContain('hljs-keyword');
  });

  it('highlights both sides of the split view identically', () => {
    const el = render(TS_DIFF, 'src/a.ts');
    TestBed.inject(DiffViewStore).setMode('split');
    fixture.detectChanges();

    const sides = el.querySelectorAll('.split .side code');
    const htmls = Array.from(sides).map((s) => s.innerHTML);
    expect(htmls.some((h) => h.includes('hljs-keyword'))).toBeTrue();
  });

  it('highlights the non-diff plain-text fallback using fileLabel', () => {
    const el = render('const value: number = 2;\n', 'src/a.ts');
    const code = el.querySelector('pre.plain code') as HTMLElement;
    expect(code.innerHTML).toContain('hljs-keyword');
    expect(code.textContent).toBe('const value: number = 2;\n');
  });
});
