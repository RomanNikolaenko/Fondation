import { Component, input, output } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { signal } from '@angular/core';

import { EngineService } from '../../core/engine.service';
import { EngineStore } from '../../core/engine-store';
import { EventsService } from '../../core/events.service';
import type { Intent } from '../../core/generated/Intent';
import { ApprovalCard } from './approval-card';
import { Approvals, deadlineFor } from './approvals';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

@Component({
  selector: 'app-approval-card',
  standalone: true,
  template: ` <button class="approve" (click)="approve.emit(intent().id)">approve</button> `,
})
class ApprovalCardStub {
  intent = input.required<Intent>();
  equity = input<string | null>(null);
  deadline = input<string | null>(null);
  approve = output<string>();
  reject = output<string>();
}

function intent(id: string): Intent {
  return {
    id,
    session_date: '2026-11-09',
    signal_date: '2026-11-08',
    symbol: 'AAPL',
    strategy: 'breakout',
    kind: {
      type: 'entry',
      qty: 100,
      limit_price: '101.50',
      stop_price: '98.00',
      take_profit: null,
      risk_usd: 1000,
      news: null,
    },
    status: 'pending_approval',
    explanation: '',
    created_at: '2026-11-09T13:15:00Z',
    updated_at: '2026-11-09T13:15:00Z',
    broker_order_id: null,
    error: null,
  };
}

function configure(api: Partial<EngineService>): void {
  const store = {
    status: signal(null),
    config: signal(null),
    pendingCount: signal(0),
    modeState: signal('running'),
    start: vi.fn(),
    stop: vi.fn(),
    refresh: vi.fn().mockResolvedValue(undefined),
  };
  TestBed.configureTestingModule({
    imports: [Approvals],
    providers: [
      { provide: EngineService, useValue: api },
      { provide: EngineStore, useValue: store },
      {
        provide: EventsService,
        useValue: { events: signal(null), start: vi.fn(), on: vi.fn(() => () => {}) },
      },
    ],
  });
  TestBed.overrideComponent(Approvals, {
    remove: { imports: [ApprovalCard] },
    add: { imports: [ApprovalCardStub] },
  });
}

describe('Approvals', () => {
  afterEach(() => TestBed.resetTestingModule());

  it('deadline countdown uses ET schedule', () => {
    expect(deadlineFor('2026-11-09', '08:55').toISOString()).toBe('2026-11-09T13:55:00.000Z');
    expect(deadlineFor('2026-10-28', '16:30').toISOString()).toBe('2026-10-28T20:30:00.000Z');
  });

  it('approve calls approveIntent and removes the card', async () => {
    const api = {
      getIntents: vi.fn().mockResolvedValue([intent('i-1'), intent('i-2')]),
      getAccount: vi.fn().mockResolvedValue({ equity: '100000' }),
      approveIntent: vi.fn().mockResolvedValue({ id: 'i-1' }),
    };
    configure(api);

    const fixture = TestBed.createComponent(Approvals);
    fixture.detectChanges();

    await vi.waitFor(() =>
      expect(
        (fixture.nativeElement as HTMLElement).querySelectorAll('app-approval-card').length,
      ).toBe(2),
    );

    const button = (fixture.nativeElement as HTMLElement).querySelector(
      '.approve',
    ) as HTMLButtonElement;
    button.click();

    await vi.waitFor(() => expect(api.approveIntent).toHaveBeenCalledWith('i-1'));
    await vi.waitFor(() =>
      expect(
        (fixture.nativeElement as HTMLElement).querySelectorAll('app-approval-card').length,
      ).toBe(1),
    );
    fixture.destroy();
  });

  it('empty state when no pending intents', async () => {
    const api = {
      getIntents: vi.fn().mockResolvedValue([]),
      getAccount: vi.fn().mockResolvedValue({ equity: '100000' }),
    };
    configure(api);

    const fixture = TestBed.createComponent(Approvals);
    fixture.detectChanges();

    await vi.waitFor(() =>
      expect((fixture.nativeElement as HTMLElement).textContent).toContain(
        'Немає угод на схвалення',
      ),
    );
    fixture.destroy();
  });
});
