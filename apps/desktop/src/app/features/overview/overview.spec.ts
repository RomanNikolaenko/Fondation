import { WritableSignal, computed, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { EngineService } from '../../core/engine.service';
import { EngineStore } from '../../core/engine-store';
import { EventsService } from '../../core/events.service';
import type { AccountDto } from '../../core/generated/AccountDto';
import type { EngineConfig } from '../../core/generated/EngineConfig';
import type { PositionDto } from '../../core/generated/PositionDto';
import type { StatusDto } from '../../core/generated/StatusDto';
import { Overview } from './overview';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

const ACCOUNT: AccountDto = {
  equity: '10000',
  cash: '5000',
  buying_power: '5000',
  day_pnl: '-50',
  week_pnl: '120',
};

const POSITION: PositionDto = {
  symbol: 'AAPL',
  strategy: 'breakout',
  qty: 10,
  entry_price: '90',
  entry_date: '2026-11-04',
  current_price: '100',
  stop_price: '85',
  take_profit: null,
  unrealized_pnl: '100',
  sector: 'Tech',
};

function status(overrides: Partial<StatusDto> = {}): StatusDto {
  return {
    engine_mode: { state: 'running', reason: null },
    account_mode: 'paper',
    execution_mode: 'approval',
    needs_setup: [],
    next_step: null,
    last_reconcile_at: null,
    version: '0.1.0',
    now: '2026-11-05T14:00:00Z',
    ...overrides,
  };
}

function config(): EngineConfig {
  return {
    risk: {
      risk_per_trade: 0.01,
      max_open_positions: 10,
      max_position_pct: 0.15,
      max_sector_pct: 0.3,
      max_gross_exposure: 1.0,
      daily_loss_limit: 0.03,
      weekly_loss_limit: 0.06,
      max_orders_per_day: 20,
      price_sanity_pct: 0.2,
      max_gap: 0.1,
    },
  } as EngineConfig;
}

interface Setup {
  fixture: ReturnType<typeof TestBed.createComponent<Overview>>;
  status: WritableSignal<StatusDto>;
}

function setup(statusSignal: WritableSignal<StatusDto>): Setup {
  const api = {
    getAccount: vi.fn().mockResolvedValue(ACCOUNT),
    getPositions: vi.fn().mockResolvedValue([POSITION]),
    getJournal: vi.fn().mockResolvedValue([]),
    pauseEngine: vi.fn().mockResolvedValue(statusSignal()),
    resumeEngine: vi.fn().mockResolvedValue(statusSignal()),
  };
  const store = {
    status: statusSignal,
    config: signal(config()),
    pendingCount: signal(0),
    modeState: computed(() => statusSignal().engine_mode.state),
    start: vi.fn(),
    stop: vi.fn(),
    refresh: vi.fn().mockResolvedValue(undefined),
  };
  const events = {
    events: signal(null),
    start: vi.fn(),
    on: vi.fn(() => () => {}),
  };

  TestBed.configureTestingModule({
    imports: [Overview],
    providers: [
      provideRouter([]),
      { provide: EngineService, useValue: api },
      { provide: EngineStore, useValue: store },
      { provide: EventsService, useValue: events },
    ],
  });

  const fixture = TestBed.createComponent(Overview);
  fixture.detectChanges();
  return { fixture, status: statusSignal };
}

function buttonTexts(fixture: Setup['fixture']): string[] {
  return Array.from(
    fixture.nativeElement.querySelectorAll('button') as NodeListOf<HTMLButtonElement>,
  ).map((b) => (b.textContent ?? '').trim());
}

describe('Overview', () => {
  afterEach(() => TestBed.resetTestingModule());

  it('shows setup warning when needs_setup is non-empty', async () => {
    const statusSignal = signal(status({ needs_setup: ['alpaca_key_id'] }));
    const { fixture } = setup(statusSignal);

    await fixture.whenStable();
    fixture.detectChanges();

    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('Додайте ключі API');
    expect(fixture.nativeElement.querySelector('a[href="/settings"]')).not.toBeNull();
  });

  it('shows Resume when paused and Pause when running', async () => {
    const statusSignal = signal(status());
    const { fixture } = setup(statusSignal);

    await fixture.whenStable();
    fixture.detectChanges();
    expect(buttonTexts(fixture)).toContain('Пауза');

    statusSignal.set(status({ engine_mode: { state: 'paused', reason: 'тижневий ліміт' } }));
    fixture.detectChanges();
    expect(buttonTexts(fixture)).toContain('Відновити');
  });
});
