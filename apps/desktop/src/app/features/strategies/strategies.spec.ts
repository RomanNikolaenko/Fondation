import { TestBed } from '@angular/core/testing';

import { EngineService } from '../../core/engine.service';
import type { EngineConfig } from '../../core/generated/EngineConfig';
import type { StrategiesConfig } from '../../core/generated/StrategiesConfig';
import { Strategies, validateStrategies } from './strategies';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

const STRATEGIES: StrategiesConfig = {
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

const CONFIG: EngineConfig = {
  execution_mode: 'approval',
  api_port: 7431,
  refresh_window_days: 30,
  schedule: {
    analysis: '07:30',
    approval_deadline: '08:55',
    submit_window_start: '09:31',
    submit_window_end: '09:55',
    reconcile_every_minutes: 5,
  },
  scanner: { min_price: 5, min_avg_dollar_volume: 5_000_000, avg_window: 30, min_history_bars: 200 },
  risk: {
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
  },
  strategies: STRATEGIES,
  news_filter: {
    enabled: true,
    model: 'claude-haiku',
    effort: 'low',
    lookback_days: 7,
    max_items: 12,
    caution_in_auto: 'allow',
    on_unavailable: 'block',
    daily_budget_usd: 1,
    monthly_budget_usd: 10,
    input_usd_per_mtok: 0.8,
    output_usd_per_mtok: 4,
  },
  earnings_filter: { enabled: true, days_ahead: 3, lookahead_calendar_days: 30, on_unavailable: 'block' },
  backtest: { slippage_bps: 2, commission_per_share: 0.005, commission_min: 1 },
  ui: { disclaimer_accepted_at: null, notifications_enabled: true },
};

describe('validateStrategies', () => {
  it('accepts a valid configuration', () => {
    expect(validateStrategies(STRATEGIES)).toEqual([]);
  });

  it('blocks allocation sum over 100%', () => {
    const broken: StrategiesConfig = {
      ...STRATEGIES,
      trend_pullback: { ...STRATEGIES.trend_pullback, allocation: 0.8 },
      breakout: { ...STRATEGIES.breakout, allocation: 0.5 },
    };
    const errors = validateStrategies(broken);
    expect(errors.some((error) => error.includes('130%'))).toBe(true);
  });

  it('requires periods to be positive', () => {
    const broken: StrategiesConfig = {
      ...STRATEGIES,
      trend_pullback: { ...STRATEGIES.trend_pullback, rsi_period: 0 },
      breakout: { ...STRATEGIES.breakout, entry_lookback: -3 },
    };
    const errors = validateStrategies(broken);
    expect(errors.length).toBe(2);
    expect(errors[0]).toContain('rsi_period');
    expect(errors[1]).toContain('Вікно пробою');
  });
});

describe('Strategies screen', () => {
  afterEach(() => TestBed.resetTestingModule());

  it('server validation details are shown', async () => {
    const api = {
      getConfig: vi.fn().mockResolvedValue(CONFIG),
      putConfig: vi.fn().mockRejectedValue({
        kind: 'http',
        status: 422,
        message: 'невірна конфігурація',
        details: ['allocations must sum to 1.0 or less'],
      }),
    };
    TestBed.configureTestingModule({
      imports: [Strategies],
      providers: [{ provide: EngineService, useValue: api }],
    });

    const fixture = TestBed.createComponent(Strategies);
    fixture.detectChanges();
    await vi.waitFor(() => {
      expect(
        (fixture.nativeElement as HTMLElement).querySelectorAll('fieldset').length,
      ).toBe(3);
    });

    (
      (fixture.nativeElement as HTMLElement).querySelector(
        'button.primary',
      ) as HTMLButtonElement
    ).click();

    await vi.waitFor(() => {
      expect(api.putConfig).toHaveBeenCalledTimes(1);
      const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
      expect(text).toContain('allocations must sum to 1.0 or less');
    });
  });
});
