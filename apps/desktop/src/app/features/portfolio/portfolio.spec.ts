import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { EngineService } from '../../core/engine.service';
import { EventsService } from '../../core/events.service';
import type { AccountDto } from '../../core/generated/AccountDto';
import type { PositionDto } from '../../core/generated/PositionDto';
import { Portfolio } from './portfolio';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

type MockFn = ReturnType<typeof vi.fn>;

interface ApiMock {
  getConfig: MockFn;
  putConfig: MockFn;
  getPositions: MockFn;
  getAccount: MockFn;
  runPortfolioRebalance: MockFn;
}

const ACCOUNT: AccountDto = {
  equity: '20000',
  cash: '8000',
  buying_power: '16000',
  day_pnl: '0',
  week_pnl: null,
};

function position(symbol: string, strategy: string, qty: number, price: string): PositionDto {
  return {
    symbol,
    strategy,
    qty,
    entry_price: price,
    entry_date: '2026-11-02',
    current_price: price,
    stop_price: '0',
    take_profit: null,
    unrealized_pnl: '0',
    sector: '',
  };
}

const POSITIONS: PositionDto[] = [
  // SPY: 10 × 500 = 5 000 (25%) при цілі 50% → дельта 5 000 ≥ порогу 1 000.
  position('SPY', 'portfolio', 10, '500'),
  // AAPL: 20 × 190 = 3 800 (19%) при цілі 20% → дельта 200 < порогу.
  position('AAPL', 'portfolio', 20, '190'),
  // QQQ: позиція портфеля поза цілями — рядок із ціллю 0%.
  position('QQQ', 'portfolio', 2, '400'),
  // Swing-позиція в портфельну таблицю не потрапляє.
  position('MSFT', 'trend_pullback', 5, '400'),
];

function apiMock(enabled = true): ApiMock {
  return {
    getConfig: vi.fn().mockResolvedValue({
      execution_mode: 'approval',
      portfolio: {
        enabled,
        rebalance: enabled ? 'monthly' : 'off',
        targets: { SPY: 0.5, AAPL: 0.2 },
        drift: 0.05,
        min_order_usd: 100,
      },
    }),
    putConfig: vi.fn().mockImplementation((value: unknown) => Promise.resolve(value)),
    getPositions: vi.fn().mockResolvedValue(POSITIONS),
    getAccount: vi.fn().mockResolvedValue(ACCOUNT),
    runPortfolioRebalance: vi.fn().mockResolvedValue({ date: '2026-12-01' }),
  };
}

function configure(api: ApiMock): void {
  TestBed.configureTestingModule({
    imports: [Portfolio],
    providers: [
      { provide: EngineService, useValue: api },
      {
        provide: EventsService,
        useValue: { events: signal(null), start: vi.fn(), on: vi.fn(() => () => {}) },
      },
    ],
  });
}

async function render(api: ApiMock) {
  configure(api);
  const fixture = TestBed.createComponent(Portfolio);
  fixture.detectChanges();
  const root = fixture.nativeElement as HTMLElement;
  await vi.waitFor(() => {
    fixture.detectChanges();
    expect(root.querySelector('table.facts tr[data-symbol="SPY"]')).not.toBeNull();
  });
  return { fixture, root };
}

function cells(root: HTMLElement, symbol: string): string[] {
  const row = root.querySelector(`table.facts tr[data-symbol="${symbol}"]`);
  return Array.from(row?.querySelectorAll('td') ?? []).map((td) => td.textContent?.trim() ?? '');
}

function type(input: HTMLInputElement, value: string): void {
  input.value = value;
  input.dispatchEvent(new Event('input'));
}

describe('Portfolio screen', () => {
  afterEach(() => TestBed.resetTestingModule());

  it('renders target vs fact table for portfolio positions only', async () => {
    const api = apiMock();
    const { root } = await render(api);

    expect(cells(root, 'SPY')).toEqual(['SPY', '50.0%', '25.0%', '-25.0%', '$5,000.00', '10']);
    expect(cells(root, 'AAPL')).toEqual(['AAPL', '20.0%', '19.0%', '-1.0%', '$200.00', '20']);
    expect(cells(root, 'QQQ')).toEqual(['QQQ', '0.0%', '4.0%', '4.0%', '−$800.00', '2']);
    expect(root.querySelector('tr[data-symbol="MSFT"]')).toBeNull();
  });

  it('highlights only deltas above the engine threshold', async () => {
    const { root } = await render(apiMock());

    const over = (symbol: string) =>
      root
        .querySelector(`table.facts tr[data-symbol="${symbol}"] td.deviation`)
        ?.classList.contains('over');
    // Поріг = max(5% × 20 000, $100) = $1 000.
    expect(over('SPY')).toBe(true);
    expect(over('AAPL')).toBe(false);
    expect(over('QQQ')).toBe(false);
  });

  it('editing a weight updates the total and blocks saving above 100%', async () => {
    const api = apiMock();
    const { fixture, root } = await render(api);

    expect(root.querySelector('.total')?.textContent).toContain('70.0%');

    type(root.querySelector('input[name="pct_SPY"]') as HTMLInputElement, '90');
    fixture.detectChanges();

    const total = root.querySelector('.total') as HTMLElement;
    expect(total.textContent).toContain('110.0%');
    expect(total.textContent).toContain('понад 100 %');
    expect((root.querySelector('button.save-button') as HTMLButtonElement).disabled).toBe(true);
    expect(cells(root, 'SPY')[1]).toBe('90.0%');
  });

  it('adds a target and saves weights as fractions', async () => {
    const api = apiMock();
    const { fixture, root } = await render(api);

    type(root.querySelector('input[name="new_ticker"]') as HTMLInputElement, ' msft ');
    type(root.querySelector('input[name="new_pct"]') as HTMLInputElement, '10');
    (root.querySelector('button.add-button') as HTMLButtonElement).click();
    fixture.detectChanges();

    expect(root.querySelector('[role="alert"]')).toBeNull();
    expect(cells(root, 'MSFT')).toEqual(['MSFT', '10.0%', '0.0%', '-10.0%', '$2,000.00', '0']);
    expect(root.querySelector('.total')?.textContent).toContain('80.0%');

    (root.querySelector('button.save-button') as HTMLButtonElement).click();
    await vi.waitFor(() => expect(api.putConfig).toHaveBeenCalledTimes(1));
    const sent = api.putConfig.mock.calls[0]?.[0] as { portfolio: { targets: object } };
    expect(sent.portfolio.targets).toEqual({ SPY: 0.5, AAPL: 0.2, MSFT: 0.1 });
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(root.textContent).toContain('Цілі збережено.');
    });
  });

  it('rejects invalid new targets with a Ukrainian message', async () => {
    const api = apiMock();
    const { fixture, root } = await render(api);

    type(root.querySelector('input[name="new_ticker"]') as HTMLInputElement, 'spy');
    type(root.querySelector('input[name="new_pct"]') as HTMLInputElement, '5');
    (root.querySelector('button.add-button') as HTMLButtonElement).click();
    fixture.detectChanges();
    expect(root.querySelector('[role="alert"]')?.textContent).toContain('SPY уже в цілях');

    type(root.querySelector('input[name="new_ticker"]') as HTMLInputElement, 'IWM');
    type(root.querySelector('input[name="new_pct"]') as HTMLInputElement, '');
    (root.querySelector('button.add-button') as HTMLButtonElement).click();
    fixture.detectChanges();
    expect(root.querySelector('[role="alert"]')?.textContent).toContain('від 0 до 100');
    expect(root.querySelector('tr[data-symbol="IWM"]')).toBeNull();
  });

  it('run queues a rebalance and shows the session date', async () => {
    const api = apiMock();
    const { fixture, root } = await render(api);

    (root.querySelector('button.run-button') as HTMLButtonElement).click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      expect(root.textContent).toContain('наміри з’являться на сесію 2026-12-01');
    });
    expect(api.runPortfolioRebalance).toHaveBeenCalledTimes(1);
  });

  it('run conflict 409 is shown as an error', async () => {
    const api = apiMock();
    api.runPortfolioRebalance.mockRejectedValue({
      kind: 'http',
      status: 409,
      message: 'Ребаланс уже виконується — дочекайтеся його завершення',
      details: [],
    });
    const { fixture, root } = await render(api);

    (root.querySelector('button.run-button') as HTMLButtonElement).click();
    await vi.waitFor(() => {
      fixture.detectChanges();
      const alert = root.querySelector('[role="alert"]')?.textContent ?? '';
      expect(alert).toContain('Помилка 409');
      expect(alert).toContain('уже виконується');
    });
    expect((root.querySelector('button.run-button') as HTMLButtonElement).disabled).toBe(false);
  });

  it('disabled portfolio blocks the run button and points to settings', async () => {
    const api = apiMock(false);
    const { root } = await render(api);

    expect((root.querySelector('button.run-button') as HTMLButtonElement).disabled).toBe(true);
    expect(root.querySelector('.disabled-hint')?.textContent).toContain('розділ «Портфель»');
  });
});
