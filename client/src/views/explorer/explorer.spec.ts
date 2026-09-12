/**
 * F6-11: Explorer's "Open in preview" action. It should hand the currently
 * selected file to `ExplorerSelectionStore` (F6-10's Preview panel picks the
 * request up from there) and open the "preview" right-drawer panel if it
 * was closed - without touching Explorer's own tree/edit state.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';

import { preloadLanguage } from '../../ui/code-highlight/code-highlight';
import { ExplorerSelectionStore } from '../../ui/right-drawer/panels/explorer-selection.store';
import { ShellStore } from '../../ui/shell/shell.store';
import { ExplorerView } from './explorer';

describe('ExplorerView "Open in preview" (F6-11)', () => {
  let fixture: ComponentFixture<ExplorerView>;
  let view: ExplorerView;
  let selection: ExplorerSelectionStore;
  let shell: ShellStore;

  beforeEach(() => {
    localStorage.clear();
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [ExplorerView],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        {
          provide: ActivatedRoute,
          // `data`/`paramMap` are walked by ShellStore.refresh().
          useValue: {
            snapshot: {
              firstChild: null,
              data: {},
              paramMap: convertToParamMap({}),
              queryParamMap: convertToParamMap({}),
            },
          },
        },
      ],
    });
    // Component logic only - ngOnInit (network calls) is not exercised here,
    // so detectChanges() is deliberately not called.
    fixture = TestBed.createComponent(ExplorerView);
    view = fixture.componentInstance;
    selection = TestBed.inject(ExplorerSelectionStore);
    shell = TestBed.inject(ShellStore);
  });

  it('does nothing without a selected file', () => {
    view.openInPreview();
    expect(selection.selectedPath()).toBeNull();
  });

  it('points the Preview panel at the selected file and opens the panel', () => {
    view.directory.set('/proj');
    view.selectedPath.set('docs/a.md');

    expect(shell.rightDrawerPanels().preview).toBeFalse();

    view.openInPreview();

    expect(selection.directory()).toBe('/proj');
    expect(selection.selectedPath()).toBe('docs/a.md');
    expect(shell.rightDrawerPanels().preview).toBeTrue();
    expect(shell.rightDrawerOpen()).toBeTrue();
  });

  it('does not re-close the panel on a second open (toggle-off guard)', () => {
    view.directory.set('/proj');
    view.selectedPath.set('docs/a.md');
    view.openInPreview();
    expect(shell.rightDrawerPanels().preview).toBeTrue();

    view.selectedPath.set('docs/b.md');
    view.openInPreview();

    expect(shell.rightDrawerPanels().preview).toBeTrue();
    expect(selection.selectedPath()).toBe('docs/b.md');
  });
});

describe('ExplorerView content-pane syntax highlighting (F7-4)', () => {
  let fixture: ComponentFixture<ExplorerView>;
  let view: ExplorerView;

  beforeEach(() => {
    localStorage.clear();
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [ExplorerView],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        {
          provide: ActivatedRoute,
          useValue: {
            snapshot: {
              firstChild: null,
              data: {},
              paramMap: convertToParamMap({}),
              queryParamMap: convertToParamMap({}),
            },
          },
        },
      ],
    });
    fixture = TestBed.createComponent(ExplorerView);
    view = fixture.componentInstance;
  });

  it('highlights the selected file by its extension', async () => {
    await preloadLanguage('rust');
    view.selectedPath.set('src/main.rs');
    view.fileContent.set('fn main() {}');

    const { html, language } = view.highlightedFileContent();
    expect(language).toBe('rust');
    expect(html).toContain('hljs-keyword');
    expect(html).toContain('main');
  });

  it('falls back to escaped, unhighlighted text for an unrecognized extension', () => {
    view.selectedPath.set('notes.txt');
    view.fileContent.set('<no-lang-for-this>');

    const { html, language } = view.highlightedFileContent();
    expect(language).toBeNull();
    expect(html).toBe('&lt;no-lang-for-this&gt;');
  });
});
