import { Pipe, PipeTransform } from '@angular/core';

import type { JournalEventDto } from '../../core/generated/JournalEventDto';

/** RFC 3339 → час Києва `YYYY-MM-DD HH:MM:SS`. */
export function atKyiv(at: string): string {
  const instant = new Date(at);
  if (Number.isNaN(instant.getTime())) {
    return '';
  }
  const parts = new Intl.DateTimeFormat('en-GB', {
    timeZone: 'Europe/Kyiv',
    hourCycle: 'h23',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  }).formatToParts(instant);
  const value = (type: string): string => parts.find((p) => p.type === type)?.value ?? '';
  return `${value('year')}-${value('month')}-${value('day')} ${value('hour')}:${value('minute')}:${value('second')}`;
}

/** Поле з `"`, `,`, `;` або переносом рядка — у лапках, `"` подвоюється. */
export function csvField(value: string): string {
  if (/["\n,]/.test(value)) {
    return `"${value.replaceAll('"', '""')}"`;
  }
  return value;
}

/** Рядки журналу → CSV із колонками at_utc,at_kyiv,kind,symbol,intent_id,message,data. */
export function toCsv(rows: JournalEventDto[]): string {
  const header = 'at_utc,at_kyiv,kind,symbol,intent_id,message,data';
  const lines = rows.map((row) =>
    [
      row.at,
      atKyiv(row.at),
      row.kind,
      row.symbol ?? '',
      row.intent_id ?? '',
      row.message,
      JSON.stringify(row.data),
    ]
      .map(csvField)
      .join(','),
  );
  return [header, ...lines].join('\n');
}

/** CSV із BOM як `Blob` для завантаження. */
export function csvBlob(rows: JournalEventDto[]): Blob {
  return new Blob(['\uFEFF' + toCsv(rows)], { type: 'text/csv;charset=utf-8' });
}

/** UTC-момент → час Києва (текстом). */
@Pipe({ name: 'kyivDate', standalone: true })
export class KyivDatePipe implements PipeTransform {
  transform(value: string | null | undefined): string {
    return value ? atKyiv(value) : '—';
  }
}
