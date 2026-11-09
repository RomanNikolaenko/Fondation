import {
  Component,
  ElementRef,
  OnDestroy,
  afterNextRender,
  effect,
  input,
  viewChild,
} from '@angular/core';
import {
  CandlestickSeries,
  ColorType,
  CrosshairMode,
  LineSeries,
  createChart,
  createSeriesMarkers,
  type IChartApi,
  type ISeriesApi,
  type ISeriesMarkersPluginApi,
  type SeriesMarker,
  type Time,
} from 'lightweight-charts';

import type { Bar } from '../core/generated/Bar';

export interface ChartMarker {
  date: string;
  kind: 'entry' | 'exit' | 'stop';
  text: string;
}

export interface ChartOverlay {
  name: string;
  points: { date: string; value: number }[];
}

export interface Candle {
  time: string;
  open: number;
  high: number;
  low: number;
  close: number;
}

/** Бари API → свічки lightweight-charts. */
export function toCandles(bars: Bar[]): Candle[] {
  return bars.map((bar) => ({
    time: bar.date,
    open: bar.open,
    high: bar.high,
    low: bar.low,
    close: bar.close,
  }));
}

/** Ковзне середнє; до заповнення вікна — `null`. */
export function sma(points: number[], period: number): (number | null)[] {
  if (period <= 0) {
    return points.map(() => null);
  }
  let sum = 0;
  return points.map((point, index) => {
    sum += point;
    if (index >= period) {
      sum -= points[index - period];
    }
    return index >= period - 1 ? sum / period : null;
  });
}

const OVERLAY_COLORS = ['#2563eb', '#d97706', '#9333ea', '#0891b2'];

function cssColor(name: string, fallback: string): string {
  if (typeof getComputedStyle !== 'function') {
    return fallback;
  }
  const value = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
  return value === '' ? fallback : value;
}

export { cssColor };

/** Свічковий графік із накладеннями та позначками угод. */
@Component({
  selector: 'fond-candle-chart',
  standalone: true,
  template: `<div #host class="chart" [style.height.px]="height()"></div>`,
  styles: `
    :host {
      display: block;
    }
    .chart {
      width: 100%;
    }
  `,
})export class CandleChart implements OnDestroy {
  readonly bars = input.required<Bar[]>();
  readonly markers = input<ChartMarker[]>([]);
  readonly overlays = input<ChartOverlay[]>([]);
  readonly height = input(320);

  private readonly host = viewChild<ElementRef<HTMLDivElement>>('host');

  private chart: IChartApi | null = null;
  private candles: ISeriesApi<'Candlestick'> | null = null;
  private lines: ISeriesApi<'Line'>[] = [];
  private markerApi: ISeriesMarkersPluginApi<Time> | null = null;
  private themeObserver: MutationObserver | null = null;

  constructor() {
    effect(() => {
      // Читаємо входи, щоб ефект перезапускався на кожну зміну.
      const bars = this.bars();
      const markers = this.markers();
      const overlays = this.overlays();
      this.render(bars, markers, overlays);
    });

    afterNextRender(() => this.create());
  }

  ngOnDestroy(): void {
    this.themeObserver?.disconnect();
    this.themeObserver = null;
    this.chart?.remove();
    this.chart = null;
  }

  private create(): void {
    const host = this.host()?.nativeElement;
    if (!host || this.chart) {
      return;
    }
    try {
      this.chart = createChart(host, {
        autoSize: true,
        layout: {
          background: { type: ColorType.Solid, color: 'transparent' },
          textColor: cssColor('--text-muted', '#5b6675'),
        },
        grid: {
          vertLines: { color: cssColor('--border', '#d9dee5') },
          horzLines: { color: cssColor('--border', '#d9dee5') },
        },
        rightPriceScale: { borderColor: cssColor('--border', '#d9dee5') },
        timeScale: { borderColor: cssColor('--border', '#d9dee5') },
        crosshair: { mode: CrosshairMode.Normal },
      });
      this.candles = this.chart.addSeries(CandlestickSeries, {
        upColor: '#16a34a',
        downColor: '#dc2626',
        borderVisible: false,
        wickUpColor: '#16a34a',
        wickDownColor: '#dc2626',
      });
      this.markerApi = createSeriesMarkers(this.candles, []);
      this.render(this.bars(), this.markers(), this.overlays());
      this.watchTheme();
    } catch (error) {
      // Без canvas (наприклад, у тестах без WebGL) графік просто не малюється.
      console.warn('Не вдалося створити графік', error);
      this.chart = null;
    }
  }

  private render(
    bars: Bar[],
    markers: ChartMarker[],
    overlays: ChartOverlay[],
  ): void {
    if (!this.chart || !this.candles) {
      return;
    }
    this.candles.setData(toCandles(bars));

    for (const line of this.lines) {
      this.chart.removeSeries(line);
    }
    this.lines = overlays.map((overlay, index) => {
      const line = this.chart!.addSeries(LineSeries, {
        color: OVERLAY_COLORS[index % OVERLAY_COLORS.length],
        lineWidth: 2,
        priceLineVisible: false,
        lastValueVisible: false,
        title: overlay.name,
      });
      line.setData(
        overlay.points.map((point) => ({
          time: point.date,
          value: point.value,
        })),
      );
      return line;
    });

    this.markerApi?.setMarkers(toSeriesMarkers(markers));
    this.chart.timeScale().fitContent();
  }

  private watchTheme(): void {
    if (typeof MutationObserver !== 'function') {
      return;
    }
    this.themeObserver = new MutationObserver(() => this.applyTheme());
    this.themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });
  }

  private applyTheme(): void {
    this.chart?.applyOptions({
      layout: { textColor: cssColor('--text-muted', '#5b6675') },
      grid: {
        vertLines: { color: cssColor('--border', '#d9dee5') },
        horzLines: { color: cssColor('--border', '#d9dee5') },
      },
      rightPriceScale: { borderColor: cssColor('--border', '#d9dee5') },
      timeScale: { borderColor: cssColor('--border', '#d9dee5') },
    });
  }
}

/** Позначки угод → маркери lightweight-charts. */
export function toSeriesMarkers(markers: ChartMarker[]): SeriesMarker<Time>[] {
  return markers.map((marker) => {
    switch (marker.kind) {
      case 'entry':
        return {
          time: marker.date,
          position: 'belowBar',
          shape: 'arrowUp',
          color: '#16a34a',
          text: marker.text,
        };
      case 'exit':
        return {
          time: marker.date,
          position: 'aboveBar',
          shape: 'arrowDown',
          color: '#dc2626',
          text: marker.text,
        };
      case 'stop':
        return {
          time: marker.date,
          position: 'inBar',
          shape: 'circle',
          color: '#d97706',
          text: marker.text,
        };
    }
  });
}
