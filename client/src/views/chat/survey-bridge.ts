import { Injectable } from '@angular/core';

/**
 * `ask_user` questionnaire (1.8): the survey card renders deep inside a
 * message row; the chat view owns the composer. The card hands its compact
 * answer line (`1A, 2BC, 3E(own words)`) to whichever chat view is on screen.
 */
@Injectable({ providedIn: 'root' })
export class SurveyBridge {
  private handler: ((text: string) => Promise<void>) | null = null;

  attach(handler: (text: string) => Promise<void>): void {
    this.handler = handler;
  }

  detach(handler: (text: string) => Promise<void>): void {
    if (this.handler === handler) {
      this.handler = null;
    }
  }

  get canSubmit(): boolean {
    return this.handler !== null;
  }

  submit(text: string): Promise<void> {
    return this.handler ? this.handler(text) : Promise.reject(new Error('no chat view'));
  }
}
