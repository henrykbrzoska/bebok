/**
 * Connect view (M6): the remote-engine connection screen used by the
 * Capacitor (mobile) shell (and browser mode). The engine is a remote process
 * reachable over LAN; this screen stores its URL and wires the SSE stream.
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router, RouterLink } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { EventsStore } from '../../core/events.store';
import { I18nService } from '../../i18n/i18n.service';

@Component({
  selector: 'app-connect',
  imports: [FormsModule, RouterLink],
  templateUrl: './connect.html',
  styleUrl: './connect.css',
})
export class ConnectView implements OnInit {
  private readonly engine = inject(EngineClient);
  private readonly events = inject(EventsStore);
  private readonly router = inject(Router);
  private readonly i18n = inject(I18nService);

  readonly t = this.i18n.t.bind(this.i18n);

  readonly baseUrl = signal('');
  readonly connecting = signal(false);
  readonly connected = signal(false);
  readonly error = signal<string | null>(null);

  ngOnInit(): void {
    const defaults = this.engine.remoteDefaults();
    this.baseUrl.set(defaults.baseUrl);
    this.connected.set(this.engine.connected());
  }

  async connect(): Promise<void> {
    const baseUrl = this.baseUrl().trim() || 'http://127.0.0.1:8787';
    this.engine.reconfigure({ kind: 'http', baseUrl: baseUrl.replace(/\/+$/, '') });
    this.connecting.set(true);
    this.error.set(null);
    try {
      await this.engine.ping();
      this.events.restart();
      this.connected.set(true);
    } catch (err) {
      this.error.set(this.describe(err));
    } finally {
      this.connecting.set(false);
    }
  }

  async continueToDirectory(): Promise<void> {
    await this.router.navigate(['/']);
  }

  describe(err: unknown): string {
    return err instanceof Error ? err.message : String(err);
  }
}
