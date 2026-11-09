import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { Router } from '@angular/router';

import { EngineService } from '../../core/engine.service';
import type { BacktestDetailDto } from '../../core/generated/BacktestDetailDto';
import type { BacktestRequest } from '../../core/generated/BacktestRequest';
import type { BacktestResult } from '../../core/generated/BacktestResult';
import type { BacktestRunDto } from '../../core/generated/BacktestRunDto';
import type { CostModel } from '../../core/generated/CostModel';
import type { Limitation } from '../../core/generated/Limitation';
import type { Metrics } from '../../core/generated/Metrics';
import type { MetricsReport } from '../../core/generated/MetricsReport';
import type { RiskConfig } from '../../core/generated/RiskConfig';
import type { ScannerConfig } from '../../core/generated/ScannerConfig';
import type { StrategiesConfig } from '../../core/generated/StrategiesConfig';
import { Backtest, defaultSplit } from './backtest';
import { EquityChart } from './equity-chart';
import { LIMITATION_TEXT, pct } from './limitations';
import { MetricsTable } from './metrics-table';

@Component({
  selector: 'fond-equity-chart',
  standalone: true,
  template: '',
  inputs: ['points', 'height'],
})
class EquityChartStub {}

const RUN_ID = 'bt-1';

function metrics(overrides: Partial<Metrics> = {}): Metrics {
  return {
    cagr: 0.123,
    max_drawdown: -0.2,
    max_drawdown_days: 30,
    sharpe: 1.1,
    sortino: 1.4,
    win_rate: 0.55,
    profit_factor: 1.6,
    avg_win: 500,
    avg_loss: -300,
    avg_hold_days: 4.2,
    trades: 100,
    exposure: 0.45,
    ...overrides,
  };
}

function report(): MetricsReport {
  return {
    full: metrics(),
    in_sample: metrics({ cagr: 0.09, trades: 70 }),
    out_of_sample: null,
    benchmark: metrics({ cagr: 0.08, trades: 0 }),
  };
}

function strategies(): StrategiesConfig {
  return {
    trend_pullback: {
      enabled: true,
      allocation: 0.6,
      sma_long: 200,
      sma_mid: 50,
      rsi_period: 14,
      rsi_entry: 45,
      atr_period: 14,
      atr_stop_mult: 2.5,
      exit_sma: 20,
      max_hold_days: 15,
    },
    breakout: {
      enabled: true,
      allocation: 0.4,
      entry_lookback: 55,
      volume_mult: 1.5,
      volume_avg: 50,
      atr_period: 14,
      atr_stop_mult: 2.5,
      trail_lookback: 10,
    },
    market_filter: { symbol: 'SPY', sma: 200 },
  };
}

function risk(): RiskConfig {
  return {
    risk_per_trade: 0.01,
    max_open_positions: 6,
    max_position_pct: 0.2,
    max_sector_pct: 0.35,
    max_gross_exposure: 1.0,
    daily_loss_limit: 0.02,
    weekly_loss_limit: 0.05,
    max_orders_per_day: 20,
    price_sanity_pct: 0.05,
    max_gap: 0.2,
  };
}

function scanner(): ScannerConfig {
  return {
    min_price: 5,
    min_avg_dollar_volume: 5_000_000,
    avg_window: 30,
    min_history_bars: 200,
  };
}

function costs(): CostModel {
  return {
    slippage_bps: 2,
    commission_per_share: 0.005,
    commission_min: 1,
  };
}

function request(): BacktestRequest {
  return {
    from: '2016-06-01',
    to: '2026-11-06',
    split: '2022-06-01',
    initial_equity: 100_000,
    strategies: strategies(),
    risk: risk(),
    scanner: scanner(),
    costs: costs(),
  };
}

function runDto(status: string): BacktestRunDto {
  return {
    id: RUN_ID,
    created_at: '2026-11-09T12:00:00Z',
    status,
    request: request(),
    error: null,
  };
}

function doneDetail(): BacktestDetailDto {
  const result: BacktestResult = {
    trades: [
      {
        symbol: 'MSFT',
        strategy: 'breakout',
        entry_date: '2025-05-06',
        entry_price: 100,
        exit_date: '2025-05-20',
        exit_price: 106,
        qty: 100,
        pnl: 580,
        exit_reason: { type: 'signal', detail: 'Трейлінг-стоп' },
      },
    ],
    equity: [{ date: '2025-05-20', equity: 100_580, benchmark: 100_000, in_market: false }],
    decisions: [],
    metrics: report(),
    limitations: ['survivorship_bias', 'news_filter_not_simulated'] as Limitation[],
  };
  return { run: runDto('done'), result };
}

function runningDetail(): BacktestDetailDto {
  return { run: runDto('running'), result: null };
}

type MockFn = ReturnType<typeof vi.fn>;

interface ApiMock {
  getConfig: MockFn;
  listBacktests: MockFn;
  startBacktest: MockFn;
  getBacktest: MockFn;
  startHistoryDownload: MockFn;
  getHistoryJob: MockFn;
}

function apiMock(overrides: Partial<ApiMock> = {}): ApiMock {
  const api: ApiMock = {
    getConfig: vi.fn().mockResolvedValue({
      strategies: strategies(),
      risk: risk(),
      scanner: scanner(),
      backtest: costs(),
    }),
    listBacktests: vi.fn().mockResolvedValue([]),
    startBacktest: vi.fn().mockResolvedValue(runDto('queued')),
    getBacktest: vi.fn().mockResolvedValue(runningDetail()),
    startHistoryDownload: vi
      .fn()
      .mockResolvedValue({ status: 'running', progress: 0, message: 'качаємо' }),
    getHistoryJob: vi
      .fn()
      .mockResolvedValue({ status: 'running', progress: 0, message: 'качаємо' }),
  };
  return Object.assign(api, overrides);
}

function configure(api: ApiMock): {
  fixture: ReturnType<typeof TestBed.createComponent<Backtest>>;
  router: { navigateByUrl: ReturnType<typeof vi.fn> };
} {
  const router = { navigateByUrl: vi.fn().mockResolvedValue(true) };
  TestBed.configureTestingModule({
    imports: [Backtest],
    providers: [
      { provide: EngineService, useValue: api },
      { provide: Router, useValue: router },
    ],
  });
  TestBed.overrideComponent(Backtest, {
    remove: { imports: [EquityChart] },
    add: { imports: [EquityChartStub] },
  });
  return {
    fixture: TestBed.createComponent(Backtest) as never,
    router,
  };
}

describe('backtest helpers', () => {
  it('maps every limitation to non-empty text', () => {
    const keys: Limitation[] = [
      'survivorship_bias',
      'news_filter_not_simulated',
      'earnings_filter_not_applied',
      'weekly_loss_approximation',
    ];
    for (const key of keys) {
      expect(LIMITATION_TEXT[key].trim().length).toBeGreaterThan(20);
    }
  });

  it('pct formats fractions', () => {
    expect(pct(0.1234)).toBe('12.34%');
    expect(pct(0.03)).toBe('3.00%');
  });

  it('defaultSplit cuts at ~70% of the range', () => {
    expect(defaultSplit('2020-01-01', '2022-01-01')).toBe('2021-05-26');
    expect(defaultSplit('bad', '2022-01-01')).toBe('2022-01-01');
  });
});

describe('MetricsTable', () => {
  afterEach(() => TestBed.resetTestingModule());

  it('formats percentages and handles missing out-of-sample row', () => {
    const fixture = TestBed.createComponent(MetricsTable);
    fixture.componentRef.setInput('report', report());
    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('12.30%');
    expect(text).toContain('-20.00%');
    expect(text).toContain('55.0%');
    expect(text).toContain('недоступно');
    expect(text).toContain('бенчмарк');
  });
});

describe('Backtest screen', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    TestBed.resetTestingModule();
  });

  it('runs a backtest, polls getBacktest and renders the done result', async () => {
    const api = apiMock({
      startBacktest: vi.fn().mockResolvedValue(runDto('queued')),
      getBacktest: vi
        .fn()
        .mockResolvedValueOnce(runningDetail())
        .mockResolvedValueOnce(doneDetail()),
      listBacktests: vi.fn().mockResolvedValue([runDto('done')]),
    });
    const { fixture } = configure(api);
    fixture.detectChanges();
    await vi.advanceTimersByTimeAsync(0);

    expect(api.getConfig).toHaveBeenCalledTimes(1);
    expect(api.listBacktests).toHaveBeenCalled();

    (
      (fixture.nativeElement as HTMLElement).querySelector('button.primary') as HTMLButtonElement
    ).click();

    await vi.advanceTimersByTimeAsync(0);
    expect(api.startBacktest).toHaveBeenCalledTimes(1);
    const sent = api.startBacktest.mock.calls[0]?.[0] as BacktestRequest;
    expect(sent.from).toBe('2016-06-01');
    expect(sent.initial_equity).toBe(100_000);
    expect(sent.strategies.trend_pullback.enabled).toBe(true);
    expect(api.getBacktest).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(2_000);
    expect(api.getBacktest).toHaveBeenCalledTimes(2);

    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('Повний');
    expect(text).toContain('MSFT');
    expect(text).toContain('S&P 500');
  });

  it('clicking a trade opens the chart with run id in query params', async () => {
    const api = apiMock({
      getBacktest: vi.fn().mockResolvedValueOnce(runningDetail()).mockResolvedValue(doneDetail()),
    });
    const { fixture, router } = configure(api);
    fixture.detectChanges();
    await vi.advanceTimersByTimeAsync(0);

    (
      (fixture.nativeElement as HTMLElement).querySelector('button.primary') as HTMLButtonElement
    ).click();

    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(2_000);
    fixture.detectChanges();

    const row = (fixture.nativeElement as HTMLElement).querySelector(
      'tbody tr.trade',
    ) as HTMLTableRowElement;
    row.click();

    expect(router.navigateByUrl).toHaveBeenCalledTimes(1);
    const url = router.navigateByUrl.mock.calls[0]?.[0] as string;
    expect(url.startsWith('/chart?')).toBe(true);
    expect(url).toContain('symbol=MSFT');
    expect(url).toContain('from=2025-05-06');
    expect(url).toContain('to=2025-05-20');
    expect(url).toContain('bt=bt-1');
  });

  it('history download polls the job until done', async () => {
    const api = apiMock({
      startHistoryDownload: vi
        .fn()
        .mockResolvedValue({ status: 'running', progress: 0.1, message: 'качаємо' }),
      getHistoryJob: vi
        .fn()
        .mockResolvedValueOnce({ status: 'running', progress: 0.4, message: '2007' })
        .mockResolvedValueOnce({ status: 'done', progress: 1, message: 'готово' }),
    });
    const { fixture } = configure(api);
    fixture.detectChanges();
    await vi.advanceTimersByTimeAsync(0);

    const history = [
      ...(fixture.nativeElement as HTMLElement).querySelectorAll('button.secondary'),
    ].find((b) => b.textContent?.includes('історію')) as HTMLButtonElement;
    history.click();

    await vi.advanceTimersByTimeAsync(0);
    expect(api.startHistoryDownload).toHaveBeenCalledWith('2016-01-01');
    // Перший запит прогресу відбувається лише тіком таймера.
    expect(api.getHistoryJob).toHaveBeenCalledTimes(0);

    await vi.advanceTimersByTimeAsync(2_000);
    expect(api.getHistoryJob).toHaveBeenCalledTimes(1);

    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('2007');

    await vi.advanceTimersByTimeAsync(2_000);
    expect(api.getHistoryJob).toHaveBeenCalledTimes(2);

    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('готово');
  });
});
