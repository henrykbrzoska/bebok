import {
  Component,
  ElementRef,
  ViewEncapsulation,
  afterRenderEffect,
  computed,
  inject,
  input,
  signal,
} from '@angular/core';

import { Part, TextPart } from '../../../core/engine.dtos';
import { renderMarkdown } from '../../../core/markdown';
import { I18nService } from '../../../i18n/i18n.service';
import { HtmlPreviewComponent } from '../../../ui/html-preview/html-preview';
import { MermaidService } from '../../../ui/mermaid/mermaid.service';
import { ExplorerSelectionStore } from '../../../ui/right-drawer/panels/explorer-selection.store';
import { ShellStore } from '../../../ui/shell/shell.store';
import { ChatSessionStore } from '../chat-session.store';

/** Extract fenced ```html blocks from raw markdown (untrusted source). */
function extractHtmlBlocks(source: string): string[] {
  const out: string[] = [];
  const lines = source.split('\n');
  let i = 0;
  while (i < lines.length) {
    const fence = /^```(html?|xhtml)\s*$/i.exec(lines[i] ?? '');
    if (fence) {
      const code: string[] = [];
      i += 1;
      while (i < lines.length && !/^```\s*$/.test(lines[i] ?? '')) {
        code.push(lines[i] ?? '');
        i += 1;
      }
      i += 1;
      out.push(code.join('\n'));
    } else {
      i += 1;
    }
  }
  return out;
}

@Component({
  selector: 'app-text-part',
  // Markdown nodes come from innerHTML and do not receive Angular's scoped
  // attributes. Scope these rules by the component element instead.
  encapsulation: ViewEncapsulation.None,
  imports: [HtmlPreviewComponent],
  template: `
    <div
      class="md"
      [innerHTML]="html()"
      (contextmenu)="onContextMenu($event)"
      (click)="onClick($event)"
      [title]="t('htmlPreview.rightClickHint')"
    ></div>
    @if (previewHtml() !== null) {
      <div class="preview-wrap">
        <app-html-preview
          [html]="previewHtml() ?? ''"
          [label]="t('htmlPreview.title')"
          (closed)="previewHtml.set(null)"
        />
      </div>
    }
  `,
  styles: `
    app-text-part .md {
      word-wrap: break-word;
      font-size: 13px;
      line-height: 1.45;
    }
    app-text-part .md p {
      margin: 0 0 4px;
    }
    app-text-part .md p:last-child {
      margin-bottom: 0;
    }
    app-text-part .md pre {
      background: var(--terminal-bg);
      border: 1px solid var(--border);
      border-radius: var(--radius-panel);
      padding: 8px 10px;
      overflow-x: auto;
      margin: 6px 0;
      font-size: var(--fs-12-5);
      line-height: 1.5;
      color: var(--code-text-strong);
    }
    app-text-part .md code {
      background: rgba(255, 255, 255, 0.07);
      border-radius: 3px;
      padding: 1px 4px;
      font-size: 0.92em;
    }
    app-text-part .md pre code {
      background: none;
      padding: 0;
    }
    app-text-part .md h2,
    app-text-part .md h3,
    app-text-part .md h4,
    app-text-part .md h5,
    app-text-part .md h6 {
      margin: 8px 0 4px;
      font-weight: 600;
      line-height: 1.3;
    }
    app-text-part .md h2 {
      font-size: 1.15em;
    }
    app-text-part .md h3 {
      font-size: 1.05em;
    }
    app-text-part .md h4,
    app-text-part .md h5,
    app-text-part .md h6 {
      font-size: 1em;
    }
    app-text-part .md ul,
    app-text-part .md ol {
      margin: 3px 0 6px;
      padding-left: 22px;
    }
    app-text-part .md a {
      color: var(--accent);
    }
    app-text-part .md .md-image {
      display: block;
      max-width: 100%;
      max-height: 420px;
      margin: 6px 0;
      border-radius: var(--radius-panel);
      border: 1px solid var(--border);
      background: var(--bg-raised);
      object-fit: contain;
    }
    app-text-part .md .mermaid-block {
      margin: 6px 0;
      padding: 8px 10px;
      border: 1px solid var(--border);
      border-radius: var(--radius-panel);
      background: var(--terminal-bg);
      overflow-x: auto;
    }
    app-text-part .md .mermaid-block[data-rendered='ok'] {
      background: var(--surface);
    }
    app-text-part .md .mermaid-svg svg {
      display: block;
      max-width: 100%;
      height: auto;
      margin: 0 auto;
    }
    app-text-part .md .mermaid-src {
      margin: 0;
      padding: 0;
      border: 0;
      background: transparent;
    }
    app-text-part .md .mermaid-error {
      margin-top: 6px;
      font-size: var(--fs-12);
      color: var(--danger);
    }
    app-text-part .md blockquote {
      margin: 4px 0;
      padding: 1px 10px;
      border-left: 3px solid var(--border);
      color: var(--fg-muted);
    }
    app-text-part .preview-wrap {
      margin-top: 8px;
      border-top: 1px dashed var(--border);
      padding-top: 8px;
    }

    /* Keep links and code readable on the muted user bubble background. */
    .bubble app-text-part .md a {
      color: inherit;
      text-decoration: underline;
    }
    .bubble app-text-part .md code {
      background: rgba(0, 0, 0, 0.14);
      color: inherit;
    }
    .bubble app-text-part .md pre {
      background: rgba(0, 0, 0, 0.2);
      border-color: rgba(0, 0, 0, 0.18);
      color: inherit;
    }
    .bubble app-text-part .md blockquote {
      border-left-color: rgba(0, 0, 0, 0.25);
      color: inherit;
    }
  `,
})
export class TextPartComponent {
  private readonly i18n = inject(I18nService);
  private readonly session = inject(ChatSessionStore);
  private readonly selection = inject(ExplorerSelectionStore);
  private readonly shell = inject(ShellStore);
  readonly t = this.i18n.t.bind(this.i18n);

  private readonly host = inject<ElementRef<HTMLElement>>(ElementRef);
  private readonly mermaid = inject(MermaidService);

  readonly part = input.required<Part>();
  private readonly textPart = computed(() => this.part() as TextPart);
  readonly html = computed(() => renderMarkdown(this.textPart().text));

  constructor() {
    // Diagrams (1.8): once the markdown is in the DOM, swap ```mermaid
    // sources for SVGs. Re-runs on every text change (streaming), rendering
    // only blocks not done yet.
    afterRenderEffect(() => {
      this.html();
      void this.mermaid.renderIn(this.host.nativeElement);
    });
  }

  /** Sandboxed preview of the first ```html block (right-click to open). */
  readonly previewHtml = signal<string | null>(null);

  /** Right-click a rendered HTML code block: open it sandboxed below. */
  onContextMenu(event: MouseEvent): void {
    const target = event.target as HTMLElement | null;
    const code = target?.closest?.('pre code');
    if (!code) {
      return;
    }
    const blocks = extractHtmlBlocks(this.textPart().text);
    if (blocks.length === 0) {
      return;
    }
    event.preventDefault();
    this.previewHtml.set(blocks[0] ?? null);
  }

  /**
   * F6-11: a scheme-less link (`class="preview-link"`, set by `core/markdown.ts`
   * for a relative path or a bare `.md` mention) opens the Preview panel
   * instead of navigating the browser away. A normal `http(s)://` link (no
   * such class) is left completely alone. The marker is a class rather than
   * a `data-*` attribute because Angular's `[innerHTML]` sanitizer strips the
   * latter.
   */
  onClick(event: MouseEvent): void {
    const target = event.target as HTMLElement | null;
    const anchor = target?.closest?.('a.preview-link') as HTMLAnchorElement | null;
    if (!anchor) {
      return;
    }
    const dir = this.session.directory();
    const href = anchor.getAttribute('href');
    if (!dir || !href) {
      return;
    }
    event.preventDefault();
    // Chat text is not itself "a file", so a relative mention is resolved
    // against the project root - the same paths the engine's own tools use.
    const path = href.replace(/^\.\//, '').replace(/^\/+/, '');
    this.selection.openInPreview(dir, path);
    if (!this.shell.rightDrawerPanels().preview) {
      this.shell.toggleRightDrawerPanel('preview');
    }
    if (!this.shell.rightDrawerOpen()) {
      this.shell.toggleRightDrawer();
    }
  }
}
