import { Pipe, PipeTransform } from '@angular/core';

import { formatEtKyiv } from '../core/tz';

/** UTC-момент → `09:00 ET · 16:00 Київ`. */
@Pipe({ name: 'etKyivTime', standalone: true })
export class EtKyivTimePipe implements PipeTransform {
  transform(value: string | null | undefined): string {
    return value ? formatEtKyiv(value) : '—';
  }
}
