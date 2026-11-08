import { Component, OnInit, computed, inject, signal } from '@angular/core';
import { ActivatedRoute } from '@angular/router';

import { errorMessage } from '../../core/api-error';
import { EngineService } from '../../core/engine.service';
import type { Bar } from '../../core/generated/Bar';
import type { JournalEventDto } from '../../core/generated/JournalEventDto';
import { todayEt } from '../../core/tz';
import {
  CandleChart,
  sma,
  type ChartMarker,
  type ChartOverlay,
} from '../../shared/candle-chart';

const DEFAULT_SYMBOL = 'SPY';

/** Рік до дати `iso` (YYYY-MM-DD). */
export function minusYear(iso: string): string {
  const [year, month, day] = iso.split('-').map(Number);
  if (![year, month, day].every((v) => Number.isFinite(v))) {
    return iso;
  }
  return `${year - 1}-${String(month).padStart(2, '0')}-${String(day).padStart(2, '0')}`;
}

function inputValue(event: Event): string {
  return (event.target as HTMLInputElement).value;
}

/** Позначка з події журналу: бік продажу — вихід, решта — вхід. */
function markerFromEvent(event: JournalEventDto): ChartMarker {
  const data = event.data;
  const side =
    typeof data === 'object' && data !== null && !Array.isArray(data)
      ? String((data as Record<string, unknown>)['side'] ?? '').toLowerCase()
      : '';
  const text = event.message;
  const isStop = /stop|стоп/i.test(text);
  return {
    date: event.at.slice(0, 10),
    kind: isStop ? 'stop' : side === 'sell' ? 'exit' : 'entry',
    text,
  };
}

/** Екран «Графік»: свічки, SMA та позначки угод. */
@Component({
  selector: 'app-chart',
  standalone: true,
  imports: [CandleChart],
  templateUrl: './chart.html',
  styleUrl: './chart.scss',
})
export class Chart implements OnInit {
  private readonly api = inject(EngineService);
  private readonly route = inject(ActivatedRoute);

  protected readonly symbol = signal(DEFAULT_SYMBOL);
  protected readonly from = signal('');
  protected readonly to = signal(todayEt());

  protected readonly bars = signal<Bar[]>([]);
  protected readonly markers = signal<ChartMarker[]>([]);
  protected readonly loading = signal(false);
  protected readonly error = signal<string | null>(null);

  private bt: string | null = null;

  protected readonly hasBacktest = computed(() => this.bt !== null);

  protected readonly overlays = computed<ChartOverlay[]>(() => {
    const bars = this.bars();
    const closes = bars.map((b) => b.close);
    return [
      this.overlay('SMA5', bars, sma(closes, 5)),
      this.overlay('SMA50', bars, sma(closes, 50)),
      this.overlay('SMA200', bars, sma(closes, 200)),
    ].filter((overlay) => overlay.points.length > 0);
  });

  ngOnInit(): void {
    const query = this.route.snapshot.queryParamMap;
    const to = query.get('to') ?? todayEt();
    this.symbol.set(query.get('symbol')?.trim().toUpperCase() || DEFAULT_SYMBOL);
    this.to.set(to);
    this.from.set(query.get('from') ?? minusYear(to));
    this.bt = query.get('bt');
    void this.load();
  }

  protected setFrom(event: Event): void {
    this.from.set(inputValue(event));
  }

  protected setTo(event: Event): void {
    this.to.set(inputValue(event));
  }

  protected setSymbol(event: Event): void {
    this.symbol.set(inputValue(event));
  }

  protected onSubmit(event: Event): void {
    event.preventDefault();
    void this.load();
  }

  protected async load(): Promise<void> {
    const symbol = this.symbol().trim().toUpperCase();
    if (symbol === '') {
      this.error.set('Вкажіть тікер');
      return;
    }
    this.symbol.set(symbol);
    this.loading.set(true);
    this.error.set(null);
    try {
      const [bars, markers] = await Promise.all([
        this.api.getBars(symbol, this.from(), this.to()),
        this.loadMarkers(symbol),
      ]);
      this.bars.set(bars);
      this.markers.set(markers);
    } catch (error) {
      this.error.set(errorMessage(error));
      this.bars.set([]);
      this.markers.set([]);
    } finally {
      this.loading.set(false);
    }
  }

  private async loadMarkers(symbol: string): Promise<ChartMarker[]> {
    if (this.bt) {
      const detail = await this.api.getBacktest(this.bt);
      const trades = (detail.result?.trades ?? []).filter(
        (trade) => trade.symbol === symbol,
      );
      return trades.flatMap((trade) => [
        {
          date: trade.entry_date,
          kind: 'entry' as const,
          text: `Вхід ${trade.entry_price.toFixed(2)}`,
        },
        {
          date: trade.exit_date,
          kind: 'exit' as const,
          text: `Вихід ${trade.exit_price.toFixed(2)}`,
        },
      ]);
    }

    const events = await this.api.getJournal({
      symbol,
      from: this.from(),
      to: this.to(),
      kinds: 'fill,order',
      limit: 200,
    });
    return events.map(markerFromEvent);
  }

  private overlay(
    name: string,
    bars: Bar[],
    values: (number | null)[],
  ): ChartOverlay {
    const points: { date: string; value: number }[] = [];
    for (let i = 0; i < bars.length; i += 1) {
      const value = values[i];
      if (value !== null && value !== undefined) {
        points.push({ date: bars[i].date, value });
      }
    }
    return { name, points };
  }
}
