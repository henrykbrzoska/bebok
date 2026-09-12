/**
 * A single terminal tab (M5): one xterm.js instance bound to one engine PTY.
 *
 * The component is rendered with `*ngIf` on the active tab, so switching tabs
 * destroys and re-creates it - which reconnects the WebSocket and replays the
 * scrollback (the PTY itself lives in the engine and keeps running).
 */

import {
  AfterViewInit,
  Component,
  ElementRef,
  EventEmitter,
  Input,
  OnDestroy,
  Output,
  ViewChild,
  inject,
} from '@angular/core';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebglAddon } from '@xterm/addon-webgl';

import { EngineClient } from '../../core/engine-client.service';

export type TerminalTabStatus = 'connecting' | 'live' | 'exited' | 'error';

const RESIZE_DEBOUNCE_MS = 100;

@Component({
  selector: 'app-terminal-tab',
  template: '<div class="xterm-screen" #screen></div>',
  styles: [
    `
      :host {
        display: block;
        height: 100%;
      }
      .xterm-screen {
        height: 100%;
        width: 100%;
      }
    `,
  ],
})
export class TerminalTab implements AfterViewInit, OnDestroy {
  @Input({ required: true }) ptyId!: string;

  @Output() status = new EventEmitter<TerminalTabStatus>();

  @ViewChild('screen') screen!: ElementRef<HTMLElement>;

  private readonly engine = inject(EngineClient);

  private term?: Terminal;
  private fit?: FitAddon;
  private ws?: WebSocket;
  private resizeObserver?: ResizeObserver;
  private resizeTimer?: number;
  private terminated = false;

  ngAfterViewInit(): void {
    void this.connect();
  }

  ngOnDestroy(): void {
    this.terminated = true;
    if (this.resizeTimer !== undefined) {
      window.clearTimeout(this.resizeTimer);
    }
    this.resizeObserver?.disconnect();
    this.ws?.close();
    this.term?.dispose();
  }

  private async connect(): Promise<void> {
    this.status.emit('connecting');
    try {
      // One-time ticket -> WebSocket URL (browsers cannot set WS headers).
      const url = await this.engine.ptyWebSocketUrl(this.ptyId);
      if (this.terminated) {
        return;
      }
      this.openTerminal();

      this.ws = new WebSocket(url);
      // Binary PTY frames arrive as ArrayBuffer; xterm decodes UTF-8 itself
      // (never pre-decode - it would split multi-byte characters).
      this.ws.binaryType = 'arraybuffer';
      this.ws.onopen = () => {
        this.status.emit('live');
        this.fitAndFocus();
      };
      this.ws.onmessage = (ev: MessageEvent) => this.onMessage(ev);
      this.ws.onclose = () => this.status.emit('exited');
      this.ws.onerror = () => this.status.emit('error');
    } catch {
      this.status.emit('error');
    }
  }

  private openTerminal(): void {
    // F2-19: the xterm surface carries the redesign's terminal palette - the
    // #0f1013 background, 12.5px/1.7 monospace body text and a blinking block
    // cursor. `green` is mapped onto --success so a shell prompt that colors
    // itself (PowerShell, oh-my-posh, bash PS1) lands on the design's green.
    const term = new Terminal({
      cursorBlink: true,
      cursorStyle: 'block',
      fontSize: 12.5,
      lineHeight: 1.7,
      fontFamily: "'JetBrains Mono', ui-monospace, 'Cascadia Mono', Menlo, Consolas, monospace",
      scrollback: 5000,
      theme: {
        background: '#0f1013',
        foreground: '#c7cbd3',
        cursor: '#eceef2',
        cursorAccent: '#0f1013',
        selectionBackground: '#2a2d35',
        green: '#5fb88a',
        brightGreen: '#8fd4ab',
        red: '#e2645f',
        brightRed: '#e2938f',
        yellow: '#e0b64a',
        brightYellow: '#f2a86f',
        white: '#eceef2',
        brightBlack: '#6b7280',
      },
    });

    const fit = new FitAddon();
    term.loadAddon(fit);

    // WebGL renderer with a canvas fallback (the constructor throws when WebGL
    // is unavailable, e.g. in some webviews).
    try {
      const webgl = new WebglAddon();
      term.loadAddon(webgl);
      webgl.onContextLoss(() => webgl.dispose());
    } catch {
      /* canvas fallback (default renderer) */
    }

    term.open(this.screen.nativeElement);
    term.onData((data) => this.sendInput(data));
    term.onResize(() => this.scheduleResize());

    this.term = term;
    this.fit = fit;

    // Re-fit when the container size changes (window resize, pane resize).
    this.resizeObserver = new ResizeObserver(() => this.fit?.fit());
    this.resizeObserver.observe(this.screen.nativeElement);
  }

  private fitAndFocus(): void {
    this.fit?.fit();
    this.term?.focus();
  }

  private onMessage(ev: MessageEvent): void {
    if (ev.data instanceof ArrayBuffer && this.term) {
      this.term.write(new Uint8Array(ev.data));
    }
  }

  /** `term.onData` -> raw bytes -> base64 JSON control frame (input). */
  private sendInput(data: string): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      return;
    }
    const bytes = new TextEncoder().encode(data);
    let binary = '';
    for (const b of bytes) {
      binary += String.fromCharCode(b);
    }
    const b64 = btoa(binary);
    this.ws.send(JSON.stringify({ type: 'input', data: b64 }));
  }

  /** Fit -> cols/rows -> debounced resize control frame. */
  private scheduleResize(): void {
    if (this.resizeTimer !== undefined) {
      return;
    }
    this.resizeTimer = window.setTimeout(() => {
      this.resizeTimer = undefined;
      this.sendResize();
    }, RESIZE_DEBOUNCE_MS);
  }

  private sendResize(): void {
    if (!this.term || !this.ws || this.ws.readyState !== WebSocket.OPEN) {
      return;
    }
    this.ws.send(
      JSON.stringify({ type: 'resize', cols: this.term.cols, rows: this.term.rows }),
    );
  }
}
