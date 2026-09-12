/**
 * F6-11: Explorer's "Open in preview" action. It should hand the currently
 * selected file to `ExplorerSelectionStore` (F6-10's Preview panel picks the
 * request up from there) and open the "preview" right-drawer panel if it
 * was closed - without touching Explorer's own tree/edit state.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { ExplorerSelectionStore } from '../../core/explorer-selection.store';
import { preloadLanguage } from '../../ui/code-highlight/code-highlight';
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

/**
 * F7-3: clicking a file in the right-drawer Explorer panel navigates here
 * and points `ExplorerSelectionStore.openRequest()` at it; this view should
 * open that file (selection + content) exactly once, then clear the request
 * so a later, unrelated visit to `/explorer` doesn't replay it.
 */
describe('ExplorerView "open in Explorer" request (F7-3)', () => {
  let fixture: ComponentFixture<ExplorerView>;
  let view: ExplorerView;
  let selection: ExplorerSelectionStore;
  let fsFile: jasmine.Spy;

  function makeFixture(directoryQueryParam: string | null): void {
    TestBed.resetTestingModule();
    fsFile = jasmine.createSpy('fsFile').and.resolveTo({ path: 'a.ts', content: 'hello' });
    TestBed.configureTestingModule({
      imports: [ExplorerView],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        {
          provide: EngineClient,
          useValue: { connected: () => true, fsFile, fsTree: () => Promise.resolve({ entries: [] }) },
        },
        {
          provide: ActivatedRoute,
          useValue: {
            snapshot: {
              firstChild: null,
              data: {},
              paramMap: convertToParamMap({}),
              queryParamMap: convertToParamMap(
                directoryQueryParam ? { directory: directoryQueryParam } : {},
              ),
            },
          },
        },
      ],
    });
    fixture = TestBed.createComponent(ExplorerView);
    view = fixture.componentInstance;
    selection = TestBed.inject(ExplorerSelectionStore);
  }

  beforeEach(() => {
    localStorage.clear();
  });

  it('opens the requested file once ngOnInit resolves the matching directory', async () => {
    makeFixture('/proj');
    selection.openInExplorer('/proj', 'src/a.ts');

    await view.ngOnInit(); // sets `directory` from the query param, then loads the tree
    await fixture.whenStable();

    expect(fsFile).toHaveBeenCalledWith('/proj', 'src/a.ts');
    expect(view.selectedPath()).toBe('src/a.ts');
    expect(view.fileContent()).toBe('hello');
    expect(selection.openRequest()).toBeNull();
  });

  it('ignores a request for a different directory than the one being viewed', async () => {
    makeFixture('/other');
    selection.openInExplorer('/proj', 'src/a.ts');

    await view.ngOnInit();
    await fixture.whenStable();

    expect(fsFile).not.toHaveBeenCalled();
    expect(view.selectedPath()).toBeNull();
    // Left pending in case the user later switches to the matching directory.
    expect(selection.openRequest()).not.toBeNull();
  });

  it('does not replay an already-consumed request on a later mount of the same store', async () => {
    // Unlike `makeFixture` (which resets the whole TestBed module, so its
    // root-provided ExplorerSelectionStore is fresh every time), this test
    // keeps one module - and so one store instance - across two `ExplorerView`
    // mounts, mimicking a real "navigate away, then back without a fresh
    // drawer click" within the same app session.
    const queryParamMap = convertToParamMap({ directory: '/proj' });
    const routeStub = {
      provide: ActivatedRoute,
      useValue: {
        snapshot: { firstChild: null, data: {}, paramMap: convertToParamMap({}), queryParamMap },
      },
    };
    fsFile = jasmine.createSpy('fsFile').and.resolveTo({ path: 'a.ts', content: 'hello' });
    TestBed.configureTestingModule({
      imports: [ExplorerView],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        {
          provide: EngineClient,
          useValue: { connected: () => true, fsFile, fsTree: () => Promise.resolve({ entries: [] }) },
        },
        routeStub,
      ],
    });
    selection = TestBed.inject(ExplorerSelectionStore);
    selection.openInExplorer('/proj', 'src/a.ts');

    const first = TestBed.createComponent(ExplorerView);
    await first.componentInstance.ngOnInit();
    await first.whenStable();
    expect(fsFile).toHaveBeenCalledTimes(1);
    expect(selection.openRequest()).toBeNull();

    // A second ExplorerView instance mounts for the same directory (e.g. the
    // user navigated away and clicked "Explorer" in the sidebar again)
    // without a fresh drawer click - the request is already consumed.
    const second = TestBed.createComponent(ExplorerView);
    await second.componentInstance.ngOnInit();
    await second.whenStable();
    expect(fsFile).toHaveBeenCalledTimes(1);
    expect(second.componentInstance.selectedPath()).toBeNull();
  });
});
