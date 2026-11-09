import type { JournalEventDto } from '../../core/generated/JournalEventDto';
import { atKyiv, csvField, csvBlob, toCsv } from './csv';

function event(overrides: Partial<JournalEventDto> = {}): JournalEventDto {
  return {
    id: 1,
    at: '2026-11-09T14:30:00Z',
    kind: 'fill',
    symbol: 'AAPL',
    intent_id: 'i-1',
    message: 'Купівля виконана',
    data: { side: 'buy' },
    ...overrides,
  };
}

describe('csv', () => {
  it('toCsv escapes quotes, commas and newlines', () => {
    const csv = toCsv([event({ message: 'a "b", c\nd' })]);

    const [header, ...rest] = csv.split('\n');
    const row = rest.join('\n');
    expect(header).toBe('at_utc,at_kyiv,kind,symbol,intent_id,message,data');
    expect(row).toBe(
      '2026-11-09T14:30:00Z,2026-11-09 16:30:00,fill,AAPL,i-1,' +
        `"a ""b"", c${'\n'}d",` +
        '"{""side"":""buy""}"',
    );
  });

  it('csvField keeps plain values unquoted', () => {
    expect(csvField('AAPL')).toBe('AAPL');
    expect(csvField('ok')).toBe('ok');
    expect(csvField('semi;colon')).toBe('semi;colon');
  });

  it('atKyiv converts UTC to Kyiv wall time', () => {
    expect(atKyiv('2026-11-09T14:30:00Z')).toBe('2026-11-09 16:30:00');
  });

  it('csvBlob prepends a UTF-8 BOM', async () => {
    const blob = csvBlob([event()]);
    const bytes = new Uint8Array(await blob.arrayBuffer());
    expect([bytes[0], bytes[1], bytes[2]]).toEqual([0xef, 0xbb, 0xbf]);
  });
});
