import { etWallTimeToUtc, formatEtKyiv, todayEt } from './tz';

describe('tz', () => {
  it('etWallTimeToUtc handles EST and EDT', () => {
    expect(etWallTimeToUtc('2026-11-05', '08:55').toISOString()).toBe('2026-11-05T13:55:00.000Z');
    expect(etWallTimeToUtc('2026-10-28', '16:30').toISOString()).toBe('2026-10-28T20:30:00.000Z');
  });

  it('etWallTimeToUtc accepts seconds', () => {
    expect(etWallTimeToUtc('2026-11-05', '08:55:30').toISOString()).toBe(
      '2026-11-05T13:55:30.000Z',
    );
  });

  it('formatEtKyiv', () => {
    expect(formatEtKyiv('2026-11-05T14:00:00Z')).toBe('09:00 ET · 16:00 Київ');
  });

  it('todayEt returns an ISO date', () => {
    expect(todayEt(new Date('2026-11-05T03:00:00Z'))).toBe('2026-11-04');
  });
});
