/**
 * F6-10: the Preview panel's in-panel navigation - open a file via
 * `ExplorerSelectionStore.openInPreview`, follow a relative link, then go
 * back. Content comes from `EngineClient.fsFile`, the same call `explorer.ts`
 * already uses (no second file-read path).
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../../core/engine-client.service';
import { PreviewPanel } from './preview-panel';
import { ExplorerSelectionStore } from './explorer-selection.store';

describe('PreviewPanel (F6-10)', () => {
  let fixture: ComponentFixture<PreviewPanel>;
  let panel: PreviewPanel;
  let selection: ExplorerSelectionStore;
  let fsFile: jasmine.Spy;

  beforeEach(() => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [PreviewPanel],
      providers: [provideZonelessChangeDetection()],
    });
    fixture = TestBed.createComponent(PreviewPanel);
    panel = fixture.componentInstance;
    selection = TestBed.inject(ExplorerSelectionStore);
    const engine = TestBed.inject(EngineClient);
    fsFile = spyOn(engine, 'fsFile').and.callFake((_directory: string, path: string) =>
      Promise.resolve({ path, content: `content of ${path}` }),
    );
    fixture.detectChanges();
  });

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
});
