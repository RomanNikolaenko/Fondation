import { TestBed } from '@angular/core/testing';
import { signal } from '@angular/core';
import { provideRouter } from '@angular/router';

import { EngineService } from '../../core/engine.service';
import { EventsService } from '../../core/events.service';
import type { PositionDto } from '../../core/generated/PositionDto';
import { Positions } from './positions';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

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

describe('Positions', () => {
  afterEach(() => {
    TestBed.resetTestingModule();
    vi.unstubAllGlobals();
  });

  it('closing a position requires confirmation', async () => {
    const api = {
      getPositions: vi.fn().mockResolvedValue([POSITION]),
      getOrders: vi.fn().mockResolvedValue([]),
      closePosition: vi.fn().mockResolvedValue({ id: 'x' }),
    };
    TestBed.configureTestingModule({
      imports: [Positions],
      providers: [
        provideRouter([]),
        { provide: EngineService, useValue: api },
        {
          provide: EventsService,
          useValue: { events: signal(null), start: vi.fn(), on: vi.fn(() => () => {}) },
        },
      ],
    });

    const fixture = TestBed.createComponent(Positions);
    fixture.detectChanges();

    const confirmMock = vi.fn(() => false);
    vi.stubGlobal('confirm', confirmMock);

    await vi.waitFor(() => {
      expect((fixture.nativeElement as HTMLElement).querySelector('tbody button')).not.toBeNull();
    });

    const close = (fixture.nativeElement as HTMLElement).querySelector(
      'tbody button',
    ) as HTMLButtonElement;
    close.click();

    await vi.waitFor(() => expect(confirmMock).toHaveBeenCalled());
    expect(api.closePosition).not.toHaveBeenCalled();

    confirmMock.mockReturnValue(true);
    close.click();

    await vi.waitFor(() => expect(api.closePosition).toHaveBeenCalledWith('AAPL'));
  });

  it('renders positions and open orders', async () => {
    const api = {
      getPositions: vi.fn().mockResolvedValue([POSITION]),
      getOrders: vi.fn().mockResolvedValue([
        {
          id: 'o-1',
          client_order_id: 'c-1',
          symbol: 'MSFT',
          side: 'buy',
          order_type: 'stop_limit',
          qty: 10,
          filled_qty: 0,
          filled_avg_price: null,
          limit_price: '101.00',
          stop_price: '98.00',
          status: 'accepted',
          legs: [],
          filled_at: null,
        },
        {
          id: 'o-2',
          client_order_id: 'c-2',
          symbol: 'MSFT2',
          side: 'sell',
          order_type: 'limit',
          qty: 5,
          filled_qty: 5,
          filled_avg_price: '100',
          limit_price: '100',
          stop_price: null,
          status: 'filled',
          legs: [],
          filled_at: null,
        },
      ]),
    };
    TestBed.configureTestingModule({
      imports: [Positions],
      providers: [
        provideRouter([]),
        { provide: EngineService, useValue: api },
        {
          provide: EventsService,
          useValue: { events: signal(null), start: vi.fn(), on: vi.fn(() => () => {}) },
        },
      ],
    });

    const fixture = TestBed.createComponent(Positions);
    fixture.detectChanges();

    await vi.waitFor(() => {
      const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
      expect(text).toContain('AAPL');
      expect(text).toContain('MSFT'); // відкритий
      expect((fixture.nativeElement as HTMLElement).querySelectorAll('tbody tr').length).toBe(2);
    });
  });
});
