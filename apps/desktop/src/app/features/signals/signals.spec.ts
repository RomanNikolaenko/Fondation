import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { EngineStore } from '../../core/engine-store';
import { EngineService } from '../../core/engine.service';
import { EventsService } from '../../core/events.service';
import type { ScanResultDto } from '../../core/generated/ScanResultDto';
import type { SignalDto } from '../../core/generated/SignalDto';
import type { StatusDto } from '../../core/generated/StatusDto';
import { lastSession, Signals } from './signals';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

const SIGNAL: SignalDto = {
  signal: {
    strategy: 'trend_pullback',
    symbol: 'AAPL',
    signal_date: '2026-11-05',
    reference_price: 100,
    stop_price: 95,
    take_profit: null,
    strength: 0.8,
    explanation: 'рух до support',
  },
  status: 'rejected',
  reject_reason: 'filter unavailable',
};

const SCAN: ScanResultDto[] = [
  { symbol: 'MSFT', passed: true, reason: null },
  { symbol: 'GOOG', passed: false, reason: 'earnings in 2 days' },
  { symbol: 'NVDA', passed: false, reason: 'earnings in 2 days' },
];

describe('Signals helpers', () => {
  it('lastSession picks Friday over weekends', () => {
    // Субота 07.11.2026 → пʼятниця 06.11; середа 04.11 → середа.
    expect(lastSession(new Date('2026-11-07T12:00:00Z'))).toBe('2026-11-06');
    expect(lastSession(new Date('2026-11-04T12:00:00Z'))).toBe('2026-11-04');
  });
});

describe('Signals screen', () => {
  afterEach(() => TestBed.resetTestingModule());

  function configure(api: Partial<EngineService>, status: StatusDto | null): void {
    const store = {
      status: signal(status),
      config: signal(null),
      pendingCount: signal(0),
      modeState: signal('running'),
      start: vi.fn(),
      stop: vi.fn(),
      refresh: vi.fn().mockResolvedValue(undefined),
    };
    TestBed.configureTestingModule({
      imports: [Signals],
      providers: [
        { provide: EngineService, useValue: api },
        { provide: EngineStore, useValue: store },
        {
          provide: EventsService,
          useValue: { events: signal(null), start: vi.fn(), on: vi.fn(() => () => {}) },
        },
      ],
    });
  }

  function status(now: string): StatusDto {
    return {
      engine_mode: { state: 'running', reason: null },
      account_mode: 'paper',
      execution_mode: 'approval',
      needs_setup: [],
      next_step: null,
      last_reconcile_at: null,
      version: '0.1.0',
      now,
    };
  }

  it('signals table shows reject reason', async () => {
    const api = {
      getSignals: vi.fn().mockResolvedValue([SIGNAL]),
      getScan: vi.fn().mockResolvedValue([]),
    };
    configure(api, status('2026-11-05T14:00:00Z'));

    const fixture = TestBed.createComponent(Signals);
    fixture.detectChanges();

    await vi.waitFor(() => {
      const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
      expect(text).toContain('filter unavailable');
      expect(text).toContain('trend_pullback');
    });
  });

  it('scan summary counts reasons', async () => {
    const api = {
      getSignals: vi.fn().mockResolvedValue([]),
      getScan: vi.fn().mockResolvedValue(SCAN),
    };
    configure(api, status('2026-11-05T14:00:00Z'));

    const fixture = TestBed.createComponent(Signals);
    fixture.detectChanges();

    await vi.waitFor(() => {
      const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
      expect(text).toContain('Пройшли: 1');
      expect(text).toContain('Відсіяні: 2');
      expect(text).toContain('earnings in 2 days: 2');
    });
  });
});
