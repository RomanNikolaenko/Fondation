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
  AreaSeries,
  ColorType,
  LineSeries,
  createChart,
  type IChartApi,
  type ISeriesApi,
} from 'lightweight-charts';

import type { EquityPoint } from '../../core/generated/EquityPoint';
import { cssColor } from '../../shared/candle-chart';

interface CurvePoint {
  time: string;
  value: number;
}

function drawdownSeries(points: EquityPoint[]): CurvePoint[] {
  let peak = 0;
  return points.map((point) => {
    peak = Math.max(peak, point.equity);
    return {
      time: point.date,
      value: peak > 0 ? point.equity / peak - 1 : 0,
    };
  });
}

/** Крива капіталу проти бенчмарку та просадки (окрема панель). */
@Component({
  selector: 'fond-equity-chart',
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
})
export class EquityChart implements OnDestroy {
  readonly points = input.required<EquityPoint[]>();
  readonly height = input(300);

  private readonly host = viewChild<ElementRef<HTMLDivElement>>('host');

  private chart: IChartApi | null = null;
  private equity: ISeriesApi<'Line'> | null = null;
  private benchmark: ISeriesApi<'Line'> | null = null;
  private drawdown: ISeriesApi<'Area'> | null = null;
  private themeObserver: MutationObserver | null = null;

  constructor() {
    effect(() => {
      const points = this.points();
      this.render(points);
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
      const borderColor = cssColor('--border', '#d9dee5');
      this.chart = createChart(host, {
        autoSize: true,
        layout: {
          background: { type: ColorType.Solid, color: 'transparent' },
          textColor: cssColor('--text-muted', '#5b6675'),
        },
        grid: {
          vertLines: { color: borderColor },
          horzLines: { color: borderColor },
        },
        rightPriceScale: { borderColor },
        timeScale: { borderColor },
      });
      this.equity = this.chart.addSeries(LineSeries, {
        color: cssColor('--accent', '#0b6e4f'),
        lineWidth: 2,
        title: 'Капітал',
      });
      this.benchmark = this.chart.addSeries(LineSeries, {
        color: cssColor('--text-muted', '#5b6675'),
        lineWidth: 2,
        priceLineVisible: false,
        lastValueVisible: false,
        title: 'SPY',
      });
      this.drawdown = this.chart.addSeries(
        AreaSeries,
        {
          lineColor: '#dc2626',
          topColor: 'rgb(220 38 38 / 30%)',
          bottomColor: 'rgb(220 38 38 / 5%)',
          title: 'Просадка',
        },
        1,
      );
      this.render(this.points());
      this.watchTheme();
    } catch (error) {
      console.warn('Не вдалося створити графік', error);
      this.chart = null;
    }
  }

  private render(points: EquityPoint[]): void {
    if (!this.chart || !this.equity || !this.benchmark || !this.drawdown) {
      return;
    }
    this.equity.setData(
      points.map((p) => ({ time: p.date, value: p.equity })),
    );
    this.benchmark.setData(
      points.map((p) => ({ time: p.date, value: p.benchmark })),
    );
    this.drawdown.setData(drawdownSeries(points));
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
    const borderColor = cssColor('--border', '#d9dee5');
    this.chart?.applyOptions({
      layout: { textColor: cssColor('--text-muted', '#5b6675') },
      grid: {
        vertLines: { color: borderColor },
        horzLines: { color: borderColor },
      },
      rightPriceScale: { borderColor },
      timeScale: { borderColor },
    });
    this.equity?.applyOptions({ color: cssColor('--accent', '#0b6e4f') });
  }
}
