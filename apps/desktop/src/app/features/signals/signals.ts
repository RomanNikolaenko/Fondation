import { Component, DestroyRef, OnInit, computed, inject, signal } from '@angular/core';

import { errorMessage } from '../../core/api-error';
import { EngineStore } from '../../core/engine-store';
import { EngineService } from '../../core/engine.service';
import { EventsService } from '../../core/events.service';
import type { ScanResultDto } from '../../core/generated/ScanResultDto';
import type { SignalDto } from '../../core/generated/SignalDto';

type ScanFilter = 'all' | 'passed' | 'rejected';

/**
 * Остання завершена сесія за Нью-Йорком: сьогодні в будній — вона,
 * у вихідні — попередня пʼятниця.
 */
export function lastSession(now: Date): string {
  const parts = new Intl.DateTimeFormat('en-CA', {
    timeZone: 'America/New_York',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
  }).formatToParts(now);
  const num = (type: string): number => Number(parts.find((p) => p.type === type)?.value ?? 0);
  const date = new Date(Date.UTC(num('year'), num('month') - 1, num('day')));
  const weekday = date.getUTCDay();
  if (weekday === 0) {
    date.setUTCDate(date.getUTCDate() - 2);
  } else if (weekday === 6) {
    date.setUTCDate(date.getUTCDate() - 1);
  }
  const pad = (v: number): string => String(v).padStart(2, '0');
  return `${date.getUTCFullYear()}-${pad(date.getUTCMonth() + 1)}-${pad(date.getUTCDate())}`;
}

function inputValue(event: Event): string {
  return (event.target as HTMLInputElement).value;
}

/** Екран «Сигнали»: таблиця сигналів дня та підсумок сканера. */
@Component({
  selector: 'app-signals',
  standalone: true,
  templateUrl: './signals.html',
  styleUrl: './signals.scss',
})
export class Signals implements OnInit {
  private readonly api = inject(EngineService);
  private readonly events = inject(EventsService);
  private readonly destroyRef = inject(DestroyRef);
  protected readonly store = inject(EngineStore);

  protected readonly date = signal('');
  protected readonly scanFilter = signal<ScanFilter>('all');

  protected readonly signals = signal<SignalDto[]>([]);
  protected readonly scan = signal<ScanResultDto[]>([]);
  protected readonly loading = signal(true);
  protected readonly error = signal<string | null>(null);

  /** Сканер, відфільтрований за станом «пройшли/відсіяні». */
  protected readonly filteredScan = computed(() => {
    const filter = this.scanFilter();
    const rows = this.scan();
    if (filter === 'all') {
      return rows;
    }
    return rows.filter((row) => row.passed === (filter === 'passed'));
  });

  /** Підсумок за причинами відсіювання. */
  protected readonly scanSummary = computed(() => {
    const counts = new Map<string, number>();
    for (const row of this.scan()) {
      if (row.passed || row.reason === null) {
        continue;
      }
      counts.set(row.reason, (counts.get(row.reason) ?? 0) + 1);
    }
    return [...counts.entries()]
      .sort((a, b) => b[1] - a[1])
      .map(([reason, count]) => ({ reason, count }));
  });

  ngOnInit(): void {
    const nowIso = this.store.status()?.now;
    this.date.set(lastSession(new Date(nowIso ?? Date.now())));
    void this.reload();
    this.destroyRef.onDestroy(this.events.on('journal', () => void this.reload()));
  }

  protected setDate(event: Event): void {
    this.date.set(inputValue(event));
    void this.reload();
  }

  protected setFilter(event: Event): void {
    const value = inputValue(event);
    this.scanFilter.set(value === 'passed' || value === 'rejected' ? value : 'all');
  }

  private async reload(): Promise<void> {
    this.loading.set(true);
    try {
      const [signals, scan] = await Promise.all([
        this.api.getSignals(this.date()),
        this.api.getScan(this.date()),
      ]);
      this.signals.set(signals);
      this.scan.set(scan);
      this.error.set(null);
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.loading.set(false);
    }
  }
}
