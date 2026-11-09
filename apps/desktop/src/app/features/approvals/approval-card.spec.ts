import { TestBed } from '@angular/core/testing';

import { EngineService } from '../../core/engine.service';
import type { Intent } from '../../core/generated/Intent';
import { ApprovalCard, formatRemaining } from './approval-card';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

function intent(overrides: Partial<Intent> = {}): Intent {
  return {
    id: 'i-1',
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
    explanation: 'Пробій опору',
    created_at: '2026-11-09T13:15:00Z',
    updated_at: '2026-11-09T13:15:00Z',
    broker_order_id: null,
    error: null,
    ...overrides,
  };
}

describe('ApprovalCard helpers', () => {
  it('formatRemaining renders mm:ss', () => {
    expect(formatRemaining(90_000)).toBe('01:30');
    expect(formatRemaining(0)).toBe('завершено');
    expect(formatRemaining(-5_000)).toBe('завершено');
  });
});

describe('ApprovalCard', () => {
  afterEach(() => TestBed.resetTestingModule());

  function setup(
    intentValue: Intent,
    equity: string | null,
    deadline: string | null,
  ): {
    nativeElement: HTMLElement;
  } {
    const api = { getBars: vi.fn().mockResolvedValue([]) };
    TestBed.configureTestingModule({
      imports: [ApprovalCard],
      providers: [{ provide: EngineService, useValue: api }],
    });
    const fixture = TestBed.createComponent(ApprovalCard);
    fixture.componentRef.setInput('intent', intentValue);
    fixture.componentRef.setInput('equity', equity);
    fixture.componentRef.setInput('deadline', deadline);
    fixture.detectChanges();
    return fixture;
  }

  it('shows the accumulated risk percent of equity', () => {
    const fixture = setup(intent(), '100000', null);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('1.00%');
  });

  it('shows news verdict badge', () => {
    const withNews = intent({
      kind: {
        type: 'entry',
        qty: 100,
        limit_price: '101.50',
        stop_price: '98.00',
        take_profit: null,
        risk_usd: 1000,
        news: { verdict: 'caution', reasons: ['звіти'], key_events: [] },
      },
    });
    const fixture = setup(withNews, '100000', null);
    const badge = (fixture.nativeElement as HTMLElement).querySelector('.badge');
    expect(badge?.textContent).toContain('обережно');
  });
});
