import { Component, computed, input } from '@angular/core';

interface DiffLine {
  cls: 'add' | 'del' | 'hunk' | 'meta' | 'ctx';
  text: string;
}

/**
 * Renders tool output that looks like a unified diff (colorized). Any other
 * text is shown verbatim in a monospace block. Diff detection is conservative:
 * only output containing `@@` hunks or `diff --git` headers is interpreted.
 */
@Component({
  selector: 'app-diff-view',
  template: `
    @if (segments(); as lines) {
      <div class="diff">
        @for (line of lines; track $index) {
          <div class="line {{ line.cls }}">
            <code>{{ line.text }}</code>
          </div>
        }
      </div>
    } @else {
      <pre class="plain"><code>{{ text() }}</code></pre>
    }
  `,
  styles: `
    .diff,
    .plain {
      background: var(--bg-raised);
      border: 1px solid var(--border);
      border-radius: var(--radius-sm);
      padding: 6px 0;
      overflow-x: auto;
      font-size: 12px;
      line-height: 1.5;
      margin: 0;
    }
    .diff .line {
      padding: 0 10px;
      white-space: pre;
    }
    .diff .add {
      background: rgba(76, 175, 125, 0.12);
    }
    .diff .del {
      background: rgba(224, 82, 96, 0.12);
    }
    .diff .hunk {
      background: rgba(91, 140, 255, 0.08);
      color: var(--accent);
    }
    .diff .meta {
      color: var(--fg-muted);
    }
    .plain {
      padding: 8px 10px;
      white-space: pre-wrap;
      word-break: break-word;
      margin: 0;
    }
    code {
      font-family: var(--mono);
    }
  `,
})
export class DiffViewComponent {
  readonly text = input<string>('');

  readonly segments = computed<DiffLine[] | null>(() => {
    const text = this.text();
    const lines = text.split('\n');
    const looksLikeDiff = lines.some(
      (l) => l.startsWith('@@') || l.startsWith('diff --git') || l.startsWith('+++ b/'),
    );
    if (!looksLikeDiff) {
      return null;
    }
    return lines.map((line): DiffLine => {
      if (line.startsWith('+') && !line.startsWith('+++')) {
        return { cls: 'add', text: line };
      }
      if (line.startsWith('-') && !line.startsWith('---')) {
        return { cls: 'del', text: line };
      }
      if (line.startsWith('@@')) {
        return { cls: 'hunk', text: line };
      }
      if (line.startsWith('diff ') || line.startsWith('+++') || line.startsWith('---')) {
        return { cls: 'meta', text: line };
      }
      return { cls: 'ctx', text: line };
    });
  });
}
