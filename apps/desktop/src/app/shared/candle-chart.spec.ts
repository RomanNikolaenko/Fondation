import type { Bar } from '../core/generated/Bar';
import { sma, toCandles } from './candle-chart';

const BAR: Bar = {
  date: '2026-11-05',
  open: 100,
  high: 105,
  low: 99,
  close: 104,
  volume: 1_000_000,
};

describe('candle chart helpers', () => {
  it('toCandles maps bar fields', () => {
    expect(toCandles([BAR])).toEqual([
      { time: '2026-11-05', open: 100, high: 105, low: 99, close: 104 },
    ]);
  });

  it('sma returns nulls until period is filled', () => {
    expect(sma([1, 2, 3, 4], 3)).toEqual([null, null, 2, 3]);
  });

  it('sma of an empty series is empty', () => {
    expect(sma([], 5)).toEqual([]);
  });
});
