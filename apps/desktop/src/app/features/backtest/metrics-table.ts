import { Component, computed, input } from '@angular/core';

import type { Metrics } from '../../core/generated/Metrics';
import type { MetricsReport } from '../../core/generated/MetricsReport';
import { pct } from './limitations';

interface Row {
  label: string;
  metrics: Metrics | null;
  benefit?: boolean;
}

function fmt(value: number, digits = 2): string {
  return value.toFixed(digits);
}

/** Таблиця метрик: повний період, навчальний, перевірочний і бенчмарк. */
@Component({
  selector: 'app-metrics-table',
  standalone: true,
  template: `
    <table>
      <thead>
        <tr>
          <th></th>
          <th>CAGR</th>
          <th>Max DD</th>
          <th>Sharpe</th>
          <th>Sortino</th>
          <th>Win rate</th>
          <th>PF</th>
          <th>Сер. виграш</th>
          <th>Сер. збиток</th>
          <th>Тривалість</th>
          <th>Угод</th>
          <th>Ринок</th>
        </tr>
      </thead>
      <tbody>
        @for (row of rows(); track row.label) {
          <tr>
            <th scope="row">
              {{ row.label }}
              @if (row.benefit) {
                <span class="tag">бенчмарк</span>
              }
            </th>
            @if (row.metrics; as m) {
              <td>{{ absPct(m.cagr) }}</td>
              <td>-{{ absPct(m.max_drawdown) }}</td>
              <td>{{ fmt(m.sharpe) }}</td>
              <td>{{ fmt(m.sortino) }}</td>
              <td>{{ pct(m.win_rate, 1) }}</td>
              <td>{{ m.profit_factor === null ? '—' : fmt(m.profit_factor) }}</td>
              <td>{{ fmt(m.avg_win) }}</td>
              <td>{{ fmt(m.avg_loss) }}</td>
              <td>{{ fmt(m.avg_hold_days, 1) }} дн</td>
              <td>{{ m.trades }}</td>
              <td>{{ pct(m.exposure, 1) }}</td>
            } @else {
              <td colspan="11" class="muted">недоступно</td>
            }
          </tr>
        }
      </tbody>
    </table>
  `,
  styles: `
    table {
      width: 100%;
      border-collapse: collapse;
      font-size: 0.88rem;
    }
    th,
    td {
      padding: 0.45rem 0.6rem;
      text-align: left;
      border-bottom: 1px solid var(--border);
      white-space: nowrap;
    }
    thead th {
      color: var(--text-muted);
      font-weight: 600;
      font-size: 0.78rem;
    }
    tbody th {
      font-weight: 600;
    }
    .muted {
      color: var(--text-muted);
    }
    .tag {
      margin-left: 0.4rem;
      padding: 0.05rem 0.4rem;
      border-radius: 999px;
      background: var(--accent-soft);
      color: var(--accent);
      font-size: 0.72rem;
    }
  `,
})
export class MetricsTable {
  readonly report = input.required<MetricsReport>();

  protected readonly rows = computed<Row[]>(() => {
    const report = this.report();
    return [
      { label: 'Повний', metrics: report.full },
      { label: 'Навчальний', metrics: report.in_sample },
      { label: 'Перевірочний', metrics: report.out_of_sample },
      { label: 'SPY', metrics: report.benchmark, benefit: true },
    ];
  });

  protected pct(value: number, digits = 2): string {
    return pct(value, digits);
  }

  protected absPct(value: number): string {
    return pct(Math.abs(value));
  }

  protected fmt(value: number, digits = 2): string {
    return fmt(value, digits);
  }
}
