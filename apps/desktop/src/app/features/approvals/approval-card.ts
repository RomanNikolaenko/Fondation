import {
  Component,
  OnDestroy,
  OnInit,
  computed,
  inject,
  input,
  output,
  signal,
} from '@angular/core';

import { EngineService } from '../../core/engine.service';
import type { Bar } from '../../core/generated/Bar';
import type { Intent } from '../../core/generated/Intent';
import { formatEtKyiv, todayEt } from '../../core/tz';
import { CandleChart } from '../../shared/candle-chart';

const CHART_BARS = 60;
const CALENDAR_DAYS = 120;
const TICK_MS = 1_000;

/** Дата на `days` календарних днів раніше (`YYYY-MM-DD`). */
export function daysBefore(iso: string, days: number): string {
  const [year, month, day] = iso.split('-').map(Number);
  if (![year, month, day].every((v) => Number.isFinite(v))) {
    return iso;
  }
  const date = new Date(Date.UTC(year, month - 1, day));
  date.setUTCDate(date.getUTCDate() - days);
  const pad = (v: number): string => String(v).padStart(2, '0');
  return `${date.getUTCFullYear()}-${pad(date.getUTCMonth() + 1)}-${pad(date.getUTCDate())}`;
}

export function formatRemaining(ms: number): string {
  if (ms <= 0) {
    return 'завершено';
  }
  const total = Math.floor(ms / 1000);
  const pad = (v: number): string => String(v).padStart(2, '0');
  return `${pad(Math.floor(total / 60))}:${pad(total % 60)}`;
}

/** Картка наміру на схвалення: міні-графік, деталі входу, дедлайн, кнопки. */
@Component({
  selector: 'app-approval-card',
  standalone: true,
  imports: [CandleChart],
  templateUrl: './approval-card.html',
  styleUrl: './approval-card.scss',
})
export class ApprovalCard implements OnInit, OnDestroy {
  private readonly api = inject(EngineService);

  readonly intent = input.required<Intent>();
  /** Equity рахунку, на якому рахуємо ризик у % (рядок з API). */
  readonly equity = input<string | null>(null);
  /** Дедлайн схвалення в UTC (ISO). */
  readonly deadline = input<string | null>(null);

  readonly approve = output<string>();
  readonly reject = output<string>();

  protected readonly chartBars = signal<Bar[]>([]);
  protected readonly now = signal(Date.now());
  protected readonly busy = signal(false);

  private timer: ReturnType<typeof setInterval> | null = null;

  protected readonly entry = computed(() => {
    const kind = this.intent().kind;
    return kind.type === 'entry' ? kind : null;
  });

  protected readonly kindLabel = computed(() => {
    const kind = this.intent().kind;
    switch (kind.type) {
      case 'entry':
        return 'Вхід';
      case 'exit':
        return `Вихід · ${kind.reason}`;
      case 'move_stop':
        return 'Перенос стопа';
      case 'rebalance':
        return kind.side === 'buy' ? 'Ребаланс · купівля' : 'Ребаланс · продаж';
    }
  });

  protected readonly news = computed(() => {
    const kind = this.intent().kind;
    return kind.type === 'entry' ? kind.news : null;
  });

  /** Ризик угоди як частка капіталу: «1.00%». */
  protected readonly riskPct = computed<string | null>(() => {
    const entry = this.entry();
    const equity = this.equity();
    if (!entry || !equity || !(Number(equity) > 0)) {
      return null;
    }
    return `${((entry.risk_usd / Number(equity)) * 100).toFixed(2)}%`;
  });

  protected readonly verdict = computed(() => this.news()?.verdict ?? null);

  protected readonly verdictLabel = computed(() => {
    switch (this.verdict()) {
      case 'allow':
        return 'дозволено';
      case 'caution':
        return 'обережно';
      case 'block':
        return 'блок';
      default:
        return null;
    }
  });

  protected readonly deadlineText = computed(() => {
    const at = this.deadline();
    if (!at) {
      return null;
    }
    const remaining = new Date(at).getTime() - this.now();
    return `до ${formatEtKyiv(at)} · залишок ${formatRemaining(remaining)}`;
  });

  ngOnInit(): void {
    this.now.set(Date.now());
    this.timer = setInterval(() => this.now.set(Date.now()), TICK_MS);
    void this.loadChart();
  }

  ngOnDestroy(): void {
    if (this.timer !== null) {
      clearInterval(this.timer);
    }
  }

  protected async doApprove(): Promise<void> {
    if (this.busy()) {
      return;
    }
    this.busy.set(true);
    try {
      this.approve.emit(this.intent().id);
    } finally {
      this.busy.set(false);
    }
  }

  protected async doReject(): Promise<void> {
    if (this.busy()) {
      return;
    }
    this.busy.set(true);
    try {
      this.reject.emit(this.intent().id);
    } finally {
      this.busy.set(false);
    }
  }

  private async loadChart(): Promise<void> {
    const to = todayEt();
    try {
      const bars = await this.api.getBars(this.intent().symbol, daysBefore(to, CALENDAR_DAYS), to);
      this.chartBars.set(bars.slice(-CHART_BARS));
    } catch {
      // Графік не критичний: без даних просто не малюємо.
      this.chartBars.set([]);
    }
  }
}
