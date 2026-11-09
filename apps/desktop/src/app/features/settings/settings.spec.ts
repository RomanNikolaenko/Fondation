import { TestBed } from '@angular/core/testing';

import { EngineService } from '../../core/engine.service';
import {
  Settings,
  kyivHint,
  riskToFraction,
  riskToPercent,
  type RiskPercentSource,
} from './settings';

type MockFn = ReturnType<typeof vi.fn>;

interface ApiMock {
  getConfig: MockFn;
  putConfig: MockFn;
  getSecrets: MockFn;
  setSecret: MockFn;
  verifySecrets: MockFn;
  getUniverse: MockFn;
  addSymbol: MockFn;
  setExcluded: MockFn;
  removeSymbol: MockFn;
  importUniverse: MockFn;
  getAutostart: MockFn;
  setAutostart: MockFn;
}

function apiMock(): ApiMock {
  return {
    getConfig: vi.fn().mockResolvedValue({
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
      scanner: {
        min_price: 5,
        min_avg_dollar_volume: 5_000_000,
        avg_window: 30,
        min_history_bars: 200,
      },
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
      strategies: {},
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
      earnings_filter: {
        enabled: true,
        days_ahead: 3,
        lookahead_calendar_days: 30,
        on_unavailable: 'block',
      },
      backtest: { slippage_bps: 2, commission_per_share: 0.005, commission_min: 1 },
      ui: { disclaimer_accepted_at: null, notifications_enabled: true },
    }),
    putConfig: vi.fn().mockImplementation((value: unknown) => Promise.resolve(value)),
    getSecrets: vi.fn().mockResolvedValue([]),
    setSecret: vi.fn(),
    verifySecrets: vi.fn(),
    getUniverse: vi.fn().mockResolvedValue([]),
    addSymbol: vi.fn(),
    setExcluded: vi.fn(),
    removeSymbol: vi.fn(),
    importUniverse: vi.fn(),
    getAutostart: vi.fn().mockResolvedValue(false),
    setAutostart: vi.fn(),
  };
}

function configure(api: ApiMock): void {
  TestBed.configureTestingModule({
    imports: [Settings],
    providers: [{ provide: EngineService, useValue: api }],
  });
}

describe('settings helpers', () => {
  it('riskPercent ⇄ fraction roundtrip', () => {
    const risk: RiskPercentSource = {
      risk_per_trade: 0.01,
      max_position_pct: 0.2,
      max_sector_pct: 0.35,
      max_gross_exposure: 1,
      daily_loss_limit: 0.02,
      weekly_loss_limit: 0.05,
      price_sanity_pct: 0.05,
      max_gap: 0.2,
    };
    const percents = riskToPercent(risk);
    expect(percents['risk_per_trade']).toBe(1);
    expect(percents['max_position_pct']).toBe(20);
    const back = riskToFraction(percents) as unknown as typeof risk;
    for (const key of Object.keys(risk) as Array<keyof RiskPercentSource>) {
      expect(back[key]).toBe(risk[key]);
    }
    expect(riskToFraction({ risk_per_trade: 1 })['risk_per_trade']).toBe(0.01);
  });

  it('kyivHint converts ET wall time', () => {
    expect(kyivHint('08:55')).toMatch(/^\d{2}:\d{2}$/);
    expect(kyivHint('бракує')).toBe('—');
  });
});

describe('Settings screen', () => {
  afterEach(() => {
    TestBed.resetTestingModule();
    vi.unstubAllGlobals();
  });

  it('switching to auto requires confirmation and does not persist', async () => {
    const api = apiMock();
    configure(api);

    const fixture = TestBed.createComponent(Settings);
    fixture.detectChanges();
    await vi.waitFor(() => expect(api.getConfig).toHaveBeenCalled());

    const confirmSpy = vi.fn(() => false);
    vi.stubGlobal('confirm', confirmSpy);

    const select = (fixture.nativeElement as HTMLElement).querySelector(
      'select[name="execution_mode"]',
    ) as HTMLSelectElement;
    select.value = 'auto';
    select.dispatchEvent(new Event('change'));

    await vi.waitFor(() => expect(confirmSpy).toHaveBeenCalled());
    expect(api.putConfig).not.toHaveBeenCalled();
    // Вибір відкотився назад.
    expect(select.value).toBe('approval');
  });

  it('risk inputs convert percent to fraction on save', async () => {
    const api = apiMock();
    configure(api);

    const fixture = TestBed.createComponent(Settings);
    fixture.detectChanges();
    await vi.waitFor(() => expect(api.getConfig).toHaveBeenCalled());
    fixture.detectChanges();

    const input = (fixture.nativeElement as HTMLElement).querySelector(
      'input[name="risk_per_trade"]',
    ) as HTMLInputElement;
    expect(input.value).toBe('1');
    input.value = '2';
    input.dispatchEvent(new Event('input'));

    (
      (fixture.nativeElement as HTMLElement).querySelector(
        'button.primary',
      ) as HTMLButtonElement
    ).click();

    await vi.waitFor(() => expect(api.putConfig).toHaveBeenCalledTimes(1));
    const sent = api.putConfig.mock.calls[0]?.[0] as unknown as {
      risk: { risk_per_trade: number };
    };
    expect(sent.risk.risk_per_trade).toBe(0.02);
  });

  it('shows Kyiv hints next to ET times', async () => {
    const api = apiMock();
    configure(api);

    const fixture = TestBed.createComponent(Settings);
    fixture.detectChanges();
    await vi.waitFor(() => {
      fixture.detectChanges();
      const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
      expect(text).toContain('Київ ≈');
    });
  });
});
