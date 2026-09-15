import { Component, computed, input } from '@angular/core';

import { Part } from '../../../core/engine.dtos';
import { ImagePartComponent } from './image-part';
import { StatusPartComponent } from './status-part';
import { SurveyPartComponent, surveyOf } from './survey-part';
import { TextPartComponent } from './text-part';
import { ThinkingPartComponent } from './thinking-part';
import { ToolPartComponent } from './tool-part';
import { UsagePartComponent } from './usage-part';

/** Maps one `Part` to its renderer (SPEC §7: parts drive the chat). */
@Component({
  selector: 'app-part-renderer',
  imports: [
    TextPartComponent,
    ThinkingPartComponent,
    ToolPartComponent,
    UsagePartComponent,
    ImagePartComponent,
    StatusPartComponent,
    SurveyPartComponent,
  ],
  template: `
    @switch (part().type) {
      @case ('text') {
        <app-text-part [part]="part()" />
      }
      @case ('thinking') {
        <app-thinking-part [part]="part()" />
      }
      @case ('tool') {
        @if (isSurvey()) {
          <app-survey-part [part]="part()" />
        } @else {
          <app-tool-part [part]="part()" [toolIndex]="toolIndex()" [taskLinks]="taskLinks()" />
        }
      }
      @case ('usage') {
        <app-usage-part [part]="part()" />
      }
      @case ('image') {
        <app-image-part [part]="part()" />
      }
      @case ('status') {
        <app-status-part [part]="part()" />
      }
    }
  `,
})
export class PartRendererComponent {
  readonly part = input.required<Part>();
  /** Ordinal of this tool part within its message (-1 for non-tool parts). */
  readonly toolIndex = input(-1);
  readonly taskLinks = input<Map<string, string>>(new Map());
  /** A completed `ask_user` call renders as the questionnaire card (1.8). */
  readonly isSurvey = computed(() => surveyOf(this.part()) !== null);
}
