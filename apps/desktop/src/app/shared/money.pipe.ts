import { Pipe, PipeTransform } from '@angular/core';

const FORMAT = new Intl.NumberFormat('en-US', {
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
});

/** Гроші з рядків API: `-1234.5` → `−$1,234.50`. */
@Pipe({ name: 'money', standalone: true })
export class MoneyPipe implements PipeTransform {
  transform(value: string | number | null | undefined): string {
    if (value === null || value === undefined || value === '') {
      return '—';
    }
    const amount = typeof value === 'string' ? Number(value) : value;
    if (!Number.isFinite(amount)) {
      return '—';
    }
    const sign = amount < 0 ? '−' : '';
    return `${sign}$${FORMAT.format(Math.abs(amount))}`;
  }
}
