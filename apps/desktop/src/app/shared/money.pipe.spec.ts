import { MoneyPipe } from './money.pipe';

describe('MoneyPipe', () => {
  const pipe = new MoneyPipe();

  it('formats positive and negative values', () => {
    expect(pipe.transform('1234.5')).toBe('$1,234.50');
    expect(pipe.transform('-1234.5')).toBe('−$1,234.50');
    expect(pipe.transform(0)).toBe('$0.00');
  });

  it('returns a dash for missing or invalid values', () => {
    expect(pipe.transform(null)).toBe('—');
    expect(pipe.transform(undefined)).toBe('—');
    expect(pipe.transform('not a number')).toBe('—');
  });
});
