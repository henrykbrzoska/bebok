import { ComponentFixture, TestBed } from '@angular/core/testing';
import { SchedulesView } from './schedules';
import { EngineClient } from '../../core/engine-client.service';
import { I18nService } from '../../i18n/i18n.service';
import { ProjectSessionsStore } from '../../ui/shell/project-sessions.store';

describe('SchedulesView', () => {
  let component: SchedulesView;
  let fixture: ComponentFixture<SchedulesView>;

  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [SchedulesView],
      providers: [
        { provide: EngineClient, useValue: {
          connect: () => Promise.resolve(),
          listSchedules: () => Promise.resolve([]),
          createSchedule: () => Promise.resolve({
            id: 'test-id',
            name: 'Test Task',
            kind: 'interval_mins',
            interval_mins: 60,
            hour: null,
            minute: null,
            weekday: null,
            prompt: 'Test prompt',
            directory: '/test/project',
            agent: 'code',
            enabled: true,
            last_run_at: null,
            last_status: 'pending',
            next_run_at: new Date().toISOString(),
          }),
          patchSchedule: () => Promise.resolve({
            id: 'test-id',
            name: 'Test Task',
            kind: 'interval_mins',
            interval_mins: 60,
            hour: null,
            minute: null,
            weekday: null,
            prompt: 'Test prompt',
            directory: '/test/project',
            agent: 'code',
            enabled: false,
            last_run_at: null,
            last_status: 'pending',
            next_run_at: new Date().toISOString(),
          }),
          deleteSchedule: () => Promise.resolve(),
          runSchedule: () => Promise.resolve({
            id: 'test-id',
            name: 'Test Task',
            kind: 'interval_mins',
            interval_mins: 60,
            hour: null,
            minute: null,
            weekday: null,
            prompt: 'Test prompt',
            directory: '/test/project',
            agent: 'code',
            enabled: true,
            last_run_at: null,
            last_status: 'running',
            next_run_at: new Date().toISOString(),
          }),
        } },
        { provide: I18nService, useValue: { t: (k: string) => k } },
        { provide: ProjectSessionsStore, useValue: {
          directory: () => '/test/project',
        } },
      ],
    }).compileComponents();

    fixture = TestBed.createComponent(SchedulesView);
    component = fixture.componentInstance;
    fixture.detectChanges();
  });

  it('should create', () => {
    expect(component).toBeTruthy();
  });

  it('should show add form when toggleForm is called', () => {
    expect(component.showAddForm()).toBeFalse();
    component.toggleForm();
    expect(component.showAddForm()).toBeTrue();
  });

  it('should close form when toggleForm is called again', () => {
    component.toggleForm();
    expect(component.showAddForm()).toBeTrue();
    component.toggleForm();
    expect(component.showAddForm()).toBeFalse();
  });

  it('should reset form when closing add form', () => {
    component.newName.set('Test');
    component.newPrompt.set('Test prompt');
    component.toggleForm();
    expect(component.newName()).toBe('');
    expect(component.newPrompt()).toBe('');
  });

  it('should have agent options from engine', () => {
    expect(component.agentOptions()).toEqual(['code', 'ask', 'plan', 'debug']);
  });

  it('should set default form values', () => {
    expect(component.newAgent()).toBe('code');
    expect(component.newMode()).toBe('interval');
    expect(component.newEnabled()).toBeTrue();
    expect(component.newIntervalMinutes()).toBe(60);
  });
});
