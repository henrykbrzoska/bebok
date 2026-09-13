/**
 * F6-10: the Preview panel's in-panel navigation - open a file via
 * `ExplorerSelectionStore.openInPreview`, follow a relative link, then go
 * back. Content comes from `EngineClient.fsFile`, the same call `explorer.ts`
 * already uses (no second file-read path).
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';

import { EngineClient } from '../../../core/engine-client.service';
import { UiPrefsStore } from '../../../core/ui-prefs.store';
import { PreviewPanel, isPreviewablePath, isImagePath } from './preview-panel';
import { ExplorerSelectionStore } from './explorer-selection.store';

describe('PreviewPanel (F6-10)', () => {
  let fixture: ComponentFixture<PreviewPanel>;
  let panel: PreviewPanel;
  let selection: ExplorerSelectionStore;
  let fsFile: jasmine.Spy;
  let fsFileBinary: jasmine.Spy;
  let fsTree: jasmine.Spy;

  beforeEach(() => {
    localStorage.clear();
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [PreviewPanel],
      providers: [provideZonelessChangeDetection(), provideRouter([])],
    });
    fixture = TestBed.createComponent(PreviewPanel);
    panel = fixture.componentInstance;
    selection = TestBed.inject(ExplorerSelectionStore);
    const engine = TestBed.inject(EngineClient);
    fsFile = spyOn(engine, 'fsFile').and.callFake((_directory: string, path: string) =>
      Promise.resolve({ path, content: `content of ${path}` }),
    );
    fsFileBinary = spyOn(engine, 'fsFileBinary').and.callFake(
      (_directory: string, path: string) =>
        Promise.resolve({ path, content: '', binary: true, media_type: 'application/octet-stream' }),
    );
    fsTree = spyOn(engine, 'fsTree').and.callFake((_directory: string, path?: string) => {
      const tree: Record<string, { name: string; path: string; is_dir: boolean }[]> = {
        '': [
          { name: 'README.md', path: 'README.md', is_dir: false },
          { name: 'main.ts', path: 'main.ts', is_dir: false },
          { name: 'docs', path: 'docs', is_dir: true },
          { name: 'node_modules', path: 'node_modules', is_dir: true },
        ],
        docs: [
          { name: 'orders.md', path: 'docs/orders.md', is_dir: false },
          { name: 'config.yaml', path: 'docs/config.yaml', is_dir: false },
        ],
        node_modules: [{ name: 'x.md', path: 'node_modules/x.md', is_dir: false }],
      };
      return Promise.resolve({ path: path ?? '', entries: tree[path ?? ''] ?? [] });
    });
    fixture.detectChanges();
  });

  async function settle(): Promise<void> {
    await fixture.whenStable();
    fixture.detectChanges();
    await fixture.whenStable();
  }

  it('shows the empty state before anything has been opened', () => {
    expect(panel.currentPath()).toBeNull();
  });

  it('opens the requested file, follows a relative link, then goes back', async () => {
    selection.openInPreview('/proj', 'docs/a.md');
    fixture.detectChanges();
    await fixture.whenStable();

    expect(panel.currentPath()).toBe('docs/a.md');
    expect(panel.content()).toBe('content of docs/a.md');
    expect(panel.canBack()).toBeFalse();
    expect(panel.canForward()).toBeFalse();

    await panel.navigate('docs/b.md');
    fixture.detectChanges();

    expect(panel.currentPath()).toBe('docs/b.md');
    expect(panel.content()).toBe('content of docs/b.md');
    expect(panel.canBack()).toBeTrue();
    expect(panel.canForward()).toBeFalse();
    expect(fsFile).toHaveBeenCalledWith('/proj', 'docs/b.md');

    await panel.back();
    fixture.detectChanges();

    expect(panel.currentPath()).toBe('docs/a.md');
    expect(panel.canBack()).toBeFalse();
    expect(panel.canForward()).toBeTrue();
  });

  it('degrades gracefully instead of crashing on a missing/unreadable target', async () => {
    fsFile.and.returnValue(Promise.reject(new Error('engine GET /fs/file -> 404: not found')));

    selection.openInPreview('/proj', 'docs/missing.md');
    fixture.detectChanges();
    await fixture.whenStable();

    expect(panel.currentPath()).toBe('docs/missing.md');
    expect(panel.error()).toContain('404');
    expect(panel.content()).toBe('');
  });

  it('re-navigates when the same file is opened again (nonce bump)', async () => {
    selection.openInPreview('/proj', 'docs/a.md');
    fixture.detectChanges();
    await fixture.whenStable();

    await panel.navigate('docs/b.md');
    fixture.detectChanges();
    expect(panel.currentPath()).toBe('docs/b.md');

    selection.openInPreview('/proj', 'docs/a.md');
    fixture.detectChanges();
    await fixture.whenStable();

    expect(panel.currentPath()).toBe('docs/a.md');
    expect(panel.canBack()).toBeFalse();
    expect(panel.canForward()).toBeFalse();
  });

  // -- F9-3 / F9-15 -----------------------------------------------------------

  it('F9-3: opening a file also reveals the Preview drawer section', () => {
    const prefs = TestBed.inject(UiPrefsStore);
    prefs.setRightDrawerOpen(false);
    prefs.setRightDrawerPanel('preview', false);
    prefs.setRightDrawerPanelCollapsed('preview', true);
    selection.openInPreview('/proj', 'docs/a.md');
    expect(prefs.rightDrawerOpen()).toBeTrue();
    expect(prefs.rightDrawerPanels().preview).toBeTrue();
    expect(prefs.rightDrawerCollapsed().preview).toBeFalse();
    expect(prefs.rightDrawerReveal()?.panel).toBe('preview');
  });

  it('F9-15: shows the file name (mono) with the full path as tooltip', async () => {
    selection.openInPreview('/proj', 'docs/nested/orders.md');
    await settle();
    expect(panel.currentName()).toBe('orders.md');
    const el = fixture.nativeElement.querySelector('[data-testid="preview-file-name"]');
    expect(el.getAttribute('title')).toBe('docs/nested/orders.md');
    expect(el.textContent).toContain('orders.md');
    expect(el.textContent).not.toContain('nested');
  });

  it('F9-15: empty state explains how to fill the panel and offers Open file', () => {
    const empty = fixture.nativeElement.querySelector('[data-testid="preview-empty"]');
    expect(empty).toBeTruthy();
    expect(empty.textContent).toContain('Open file');
    expect(empty.querySelector('button')).toBeTruthy();
  });

  it('F9-15: Pin keeps the file - a replacing request is parked with "Open anyway"', async () => {
    selection.openInPreview('/proj', 'docs/a.md');
    await settle();
    panel.togglePin();
    await settle();
    expect(panel.pinned()).toBeTrue();

    selection.openInPreview('/proj', 'docs/b.md');
    await settle();
    expect(panel.currentPath()).toBe('docs/a.md');
    expect(panel.blockedRequest()?.path).toBe('docs/b.md');
    expect(fixture.nativeElement.querySelector('[data-testid="preview-blocked"]')).toBeTruthy();

    // A relative link inside the document is parked the same way.
    await panel.navigate('docs/c.md');
    expect(panel.currentPath()).toBe('docs/a.md');
    expect(panel.blockedRequest()?.path).toBe('docs/c.md');

    panel.openBlocked();
    await settle();
    expect(panel.pinned()).toBeFalse();
    expect(panel.currentPath()).toBe('docs/c.md');
    expect(panel.blockedRequest()).toBeNull();
  });

  it('F9-15: Refresh re-reads the file and Open in Explorer hands over the selection', async () => {
    selection.openInPreview('/proj', 'docs/a.md');
    await settle();
    expect(fsFile).toHaveBeenCalledTimes(1);
    await panel.refresh();
    expect(fsFile).toHaveBeenCalledTimes(2);

    const router = TestBed.inject(Router);
    const navigate = spyOn(router, 'navigate').and.resolveTo(true);
    await panel.openInExplorer();
    expect(selection.openRequest()?.path).toBe('docs/a.md');
    expect(navigate).toHaveBeenCalledWith(['/explorer'], { queryParams: { directory: '/proj' } });
  });

  it('F9-15: flags a file that changed on disk and clears the flag on Reload', async () => {
    selection.openInPreview('/proj', 'docs/a.md');
    await settle();
    expect(panel.modified()).toBeFalse();
    await panel.checkModified(true);
    expect(panel.modified()).toBeFalse(); // unchanged content -> no badge

    fsFile.and.callFake((_d: string, path: string) =>
      Promise.resolve({ path, content: `NEW content of ${path}` }),
    );
    await panel.checkModified(true);
    await settle();
    expect(panel.modified()).toBeTrue();
    expect(fixture.nativeElement.querySelector('[data-testid="preview-modified"]')).toBeTruthy();
    expect(fixture.nativeElement.querySelector('[data-testid="preview-reload"]')).toBeTruthy();

    await panel.refresh();
    await settle();
    expect(panel.modified()).toBeFalse();
    expect(panel.content()).toBe('NEW content of docs/a.md');
  });

  it('F9-15: the picker walks the tree (skipping node_modules), filters and opens a file', async () => {
    selection.select('/proj', null);
    panel.openPicker();
    await settle();
    await panel.loadPickerFiles('/proj');
    await settle();
    expect(fsTree).toHaveBeenCalledWith('/proj', undefined);
    expect(fsTree).toHaveBeenCalledWith('/proj', 'docs');
    expect(fsTree).not.toHaveBeenCalledWith('/proj', 'node_modules');
    expect(panel.pickerResults()).toEqual(['README.md', 'docs/config.yaml', 'docs/orders.md']);

    panel.onPickerQuery('ord');
    expect(panel.pickerResults()).toEqual(['docs/orders.md']);

    panel.onPickerKey(new KeyboardEvent('keydown', { key: 'Enter' }));
    await settle();
    expect(panel.pickerOpen()).toBeFalse();
    expect(panel.currentPath()).toBe('docs/orders.md');
    expect(panel.content()).toBe('content of docs/orders.md');
  });

  it('F9-15: only previewable text types are offered', () => {
    expect(isPreviewablePath('a/b.md')).toBeTrue();
    expect(isPreviewablePath('x.YAML')).toBeTrue();
    expect(isPreviewablePath('index.html')).toBeTrue();
    expect(isPreviewablePath('logo.png')).toBeTrue();
    expect(isPreviewablePath('photo.jpg')).toBeTrue();
    expect(isPreviewablePath('main.ts')).toBeFalse();
    expect(isPreviewablePath('Makefile')).toBeFalse();
  });

  it('isImagePath identifies image extensions', () => {
    expect(isImagePath('logo.png')).toBeTrue();
    expect(isImagePath('photo.JPG')).toBeTrue();
    expect(isImagePath('icon.svg')).toBeTrue();
    expect(isImagePath('doc.md')).toBeFalse();
    expect(isImagePath('data.json')).toBeFalse();
    expect(isImagePath('Makefile')).toBeFalse();
  });

  it('loads an image via fsFileBinary and exposes imageUrl', async () => {
    fsFileBinary.and.callFake(
      (_directory: string, path: string) =>
        Promise.resolve({
          path,
          content: 'iVBORw0KGgo',
          binary: true,
          media_type: 'image/png',
        }),
    );

    selection.openInPreview('/proj', 'assets/logo.png');
    await settle();

    expect(fsFileBinary).toHaveBeenCalledWith('/proj', 'assets/logo.png');
    expect(panel.isImagePreview()).toBeTrue();
    expect(panel.imageData()).toBe('iVBORw0KGgo');
    expect(panel.imageMime()).toBe('image/png');
    expect(panel.imageUrl()).toBe('data:image/png;base64,iVBORw0KGgo');
    expect(panel.content()).toBe('');

    const img = fixture.nativeElement.querySelector('[data-testid="preview-image"]');
    expect(img).toBeTruthy();
    expect(img.getAttribute('src')).toBe('data:image/png;base64,iVBORw0KGgo');
  });
});
