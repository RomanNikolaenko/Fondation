import { computed, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { EngineStore } from '../core/engine-store';
import type { StatusDto } from '../core/generated/StatusDto';
import { Shell } from './shell';

const STATUS: StatusDto = {
  engine_mode: { state: 'running', reason: null },
  account_mode: 'paper',
  execution_mode: 'approval',
  needs_setup: [],
  next_step: null,
  last_reconcile_at: null,
  version: '0.1.0',
  now: '2026-11-05T14:00:00Z',
};

describe('Shell', () => {
  afterEach(() => TestBed.resetTestingModule());

  it('shows PAPER banner and 10 nav items', async () => {
    const mode = signal('running');
    const store = {
      status: signal<StatusDto | null>(STATUS),
      config: signal(null),
      pendingCount: signal(2),
      modeState: computed(() => mode()),
      start: vi.fn(),
      stop: vi.fn(),
      refresh: vi.fn(),
    };

    TestBed.configureTestingModule({
      imports: [Shell],
      providers: [provideRouter([]), { provide: EngineStore, useValue: store }],
    });

    const fixture = TestBed.createComponent(Shell);
    await fixture.whenStable();
    fixture.detectChanges();

    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('PAPER — віртуальні гроші');
    expect(fixture.nativeElement.querySelectorAll('a.nav-link').length).toBe(10);
    expect(fixture.nativeElement.querySelector('.badge')?.textContent).toContain('2');
    expect(store.start).toHaveBeenCalled();
  });
});
