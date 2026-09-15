import { Injectable } from '@angular/core';

/**
 * Mermaid diagrams in chat (1.8). `renderMarkdown` leaves every ```mermaid
 * fence as `<div class="mermaid-block">` with the escaped source in a
 * `<pre><code>`; this swaps the source for the rendered SVG. The library is
 * bundled (no runtime download) but imported lazily, so it only loads for
 * a chat that actually contains a diagram. A diagram that fails to parse
 * keeps its source and gets the error text underneath - never a blank box.
 */
@Injectable({ providedIn: 'root' })
export class MermaidService {
  private lib: Promise<typeof import('mermaid').default> | null = null;
  private seq = 0;

  private load(): Promise<typeof import('mermaid').default> {
    this.lib ??= import('mermaid').then((m) => {
      const mermaid = m.default;
      mermaid.initialize({
        startOnLoad: false,
        theme: 'dark',
        securityLevel: 'strict',
        fontFamily: 'inherit',
      });
      return mermaid;
    });
    return this.lib;
  }

  /** Render every not-yet-rendered block under `root`. Safe to call repeatedly. */
  async renderIn(root: HTMLElement): Promise<void> {
    const blocks = Array.from(
      root.querySelectorAll<HTMLElement>('.mermaid-block:not([data-rendered])'),
    );
    if (blocks.length === 0) {
      return;
    }
    const mermaid = await this.load();
    for (const block of blocks) {
      if (block.dataset['rendered']) {
        continue;
      }
      block.dataset['rendered'] = 'pending';
      const source = block.querySelector('code')?.textContent ?? '';
      try {
        const id = `bebok-mermaid-${++this.seq}`;
        const { svg, bindFunctions } = await mermaid.render(id, source);
        const host = document.createElement('div');
        host.className = 'mermaid-svg';
        host.innerHTML = svg;
        block.replaceChildren(host);
        bindFunctions?.(host);
        block.dataset['rendered'] = 'ok';
      } catch (err) {
        const note = document.createElement('div');
        note.className = 'mermaid-error';
        note.textContent = `mermaid: ${err instanceof Error ? err.message.split('\n')[0] : String(err)}`;
        block.appendChild(note);
        block.dataset['rendered'] = 'error';
        // mermaid leaves a stray error element behind on failure.
        document.querySelector(`#dbebok-mermaid-${this.seq}`)?.remove();
      }
    }
  }
}
