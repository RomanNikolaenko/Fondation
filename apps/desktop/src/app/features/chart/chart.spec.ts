import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';

import { EngineService } from '../../core/engine.service';
import type { Bar } from '../../core/generated/Bar';
import { CandleChart } from '../../shared/candle-chart';
import { Chart } from './chart';

@Component({
  selector: 'fond-candle-chart',
  standalone: true,
  template: '',
  inputs: ['bars', 'markers', 'overlays', 'height'],
})
class CandleChartStub {}

const BARS: Bar[] = [
  { date: '2025-01-02', open: 100, high: 102, low: 99, close: 101, volume: 1_000 },
  { date: '2025-01-03', open: 101, high: 103, low: 100, close: 102, volume: 1_000 },
];

describe('Chart screen', () => {
  afterEach(() => TestBed.resetTestingModule());

  it('reads symbol from query params and loads bars', async () => {
    const api = {
      getBars: vi.fn().mockResolvedValue(BARS),
      getJournal: vi.fn().mockResolvedValue([]),
    };
    const route = {
      snapshot: {
        queryParamMap: convertToParamMap({
          symbol: 'aapl',
          from: '2025-01-01',
          to: '2026-01-01',
        }),
      },
    };

    TestBed.configureTestingModule({
      imports: [Chart],
      providers: [
        provideRouter([]),
        { provide: EngineService, useValue: api },
        { provide: ActivatedRoute, useValue: route },
      ],
    });
    TestBed.overrideComponent(Chart, {
      remove: { imports: [CandleChart] },
      add: { imports: [CandleChartStub] },
    });

    const fixture = TestBed.createComponent(Chart);
    fixture.detectChanges();
    await fixture.whenStable();

    expect(api.getBars).toHaveBeenCalledWith('AAPL', '2025-01-01', '2026-01-01');
    expect(api.getJournal).toHaveBeenCalledWith(
      expect.objectContaining({ symbol: 'AAPL', from: '2025-01-01', to: '2026-01-01' }),
    );
    const chart = fixture.componentInstance as unknown as {
      bars: () => Bar[];
    };
    await vi.waitFor(() => expect(chart.bars()).toEqual(BARS));
  });
});
