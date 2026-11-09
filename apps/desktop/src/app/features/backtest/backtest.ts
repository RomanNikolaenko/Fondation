import {
  Component,
  DestroyRef,
  OnInit,
  computed,
  inject,
  signal,
} from '@angular/core';
import { Router } from '@angular/router';

import { errorMessage } from '../../core/api-error';
import { EngineService } from '../../core/engine.service';
import type { BacktestDetailDto } from '../../core/generated/BacktestDetailDto';
import type { BacktestRequest } from '../../core/generated/BacktestRequest';
import type { BacktestRunDto } from '../../core/generated/BacktestRunDto';
import type { StrategiesConfig } from '../../core/generated/StrategiesConfig';
import type { HistoryJobDto } from '../../core/generated/HistoryJobDto';
import { lastSession } from '../signals/signals';
import { KyivDatePipe } from '../journal/csv';
import { EquityChart } from './equity-chart';
import { MetricsTable } from './metrics-table';
import { LIMITATION_TEXT, pct } from './limitations';

const HISTORY_EPOCH = '2016-01-01';
const DEFAULT_FROM = '2016-06-01';
const POLL_MS = 2_000;

function inputValue(event: Event): string {
  return (event.target as HTMLInputElement).value;
}

function numValue(event: Event): number {
  return Number(inputValue(event));
}

function boolValue(event: Event): boolean {
  return (event.target as HTMLInputElement).checked;
}

/** Дата розділення за замовчуванням: ~70% точок між `from` і `to`. */
export function defaultSplit(from: string, to: string): string {
  const [fy, fm, fd] = from.split('-').map(Number);
  const [ty, tm, td] = to.split('-').map(Number);
  if (![fy, fm, fd, ty, tm, td].every(Number.isFinite)) {
    return to;
  }
  const fromMs = Date.UTC(fy, fm - 1, fd);
  const toMs = Date.UTC(ty, tm - 1, td);
  const split = new Date(fromMs + (toMs - fromMs) * 0.7);
  const pad = (v: number): string => String(v).padStart(2, '0');
  return `${split.getUTCFullYear()}-${pad(split.getUTCMonth() + 1)}-${pad(split.getUTCDate())}`;
}

/** Екран «Бектест»: форма, запуск, результати та історія прогонів. */
@Component({
  selector: 'app-backtest',
  standalone: true,
  imports: [MetricsTable, EquityChart, KyivDatePipe],
  templateUrl: './backtest.html',
  styleUrl: './backtest.scss',
})
export class Backtest implements OnInit {
  private readonly api = inject(EngineService);
  private readonly router = inject(Router);
  private readonly destroyRef = inject(DestroyRef);

  protected readonly from = signal(DEFAULT_FROM);
  protected readonly to = signal('');
  protected readonly split = signal('');
  protected readonly initialEquity = signal(100_000);

  protected readonly runStatus = signal<string | null>(null);
  protected readonly historyJob = signal<HistoryJobDto | null>(null);
  protected readonly detail = signal<BacktestDetailDto | null>(null);
  protected readonly runs = signal<BacktestRunDto[]>([]);

  /** Параметри стратегій, сканера, ризику та витрат із конфігурації. */
  protected readonly strategies = signal<StrategiesConfig | null>(null);
  protected readonly risk = signal<BacktestRequest['risk'] | null>(null);
  protected readonly scanner = signal<BacktestRequest['scanner'] | null>(null);
  protected readonly costs = signal<BacktestRequest['costs'] | null>(null);

  protected readonly loading = signal(true);
  protected readonly error = signal<string | null>(null);
  protected readonly message = signal<string | null>(null);

  private runTimer: ReturnType<typeof setInterval> | null = null;
  private historyTimer: ReturnType<typeof setInterval> | null = null;
  private currentId: string | null = null;

  protected readonly limitations = computed(() => {
    const detail = this.detail();
    return detail?.result?.limitations.map((kind) => LIMITATION_TEXT[kind]) ?? [];
  });

  ngOnInit(): void {
    const to = this.currentTo();
    this.to.set(to);
    this.split.set(defaultSplit(DEFAULT_FROM, to));
    this.destroyRef.onDestroy(() => this.clearTimers());
    void this.reloadRuns();
    void this.loadProfile();
  }

  private async loadProfile(): Promise<void> {
    try {
      const config = await this.api.getConfig();
      this.strategies.set(config.strategies);
      this.risk.set(config.risk);
      this.scanner.set(config.scanner);
      this.costs.set(config.backtest);
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  protected currentTo(): string {
    // Рушій доступний не завжди; тоді беремо сьогодні за Нью-Йорком.
    const nowIso = Date.now();
    return lastSession(new Date(nowIso));
  }

  // ------------------------------------------------------------ форма

  protected setFrom(event: Event): void {
    this.from.set(inputValue(event));
  }

  protected setTo(event: Event): void {
    this.to.set(inputValue(event));
    this.split.set(defaultSplit(this.from(), this.to()));
  }

  protected setSplit(event: Event): void {
    this.split.set(inputValue(event));
  }

  protected setEquity(event: Event): void {
    this.initialEquity.set(Math.max(1_000, numValue(event) || 0));
  }

  /** Локальний перемикач стратегії (впливає лише на наступний запуск). */
  protected toggleStrategy(
    name: 'trend_pullback' | 'breakout',
    event: Event,
  ): void {
    this.strategies.update((current) => {
      if (!current) {
        return current;
      }
      const next = { ...current };
      if (name === 'trend_pullback') {
        next.trend_pullback = {
          ...current.trend_pullback,
          enabled: boolValue(event),
        };
      } else {
        next.breakout = { ...current.breakout, enabled: boolValue(event) };
      }
      return next;
    });
  }

  protected pct(value: number): string {
    return pct(value, 2);
  }

  /** Поточний запит: гілки стратегій, ризик і витрати — з форми/конфігурації. */
  protected async request(): Promise<BacktestRequest | null> {
    const strategies = this.strategies();
    const risk = this.risk();
    const scanner = this.scanner();
    const costs = this.costs();
    if (!strategies || !risk || !scanner || !costs) {
      this.error.set('Конфігурація ще не завантажена');
      return null;
    }
    const split = this.split();
    return {
      from: this.from(),
      to: this.to(),
      split: split === '' ? null : split,
      initial_equity: this.initialEquity(),
      strategies,
      risk,
      scanner,
      costs,
    };
  }

  // ------------------------------------------------------------ дії

  protected async downloadHistory(): Promise<void> {
    try {
      const job = await this.api.startHistoryDownload(HISTORY_EPOCH);
      this.historyJob.set(job);
      this.clearHistoryTimer();
      this.historyTimer = setInterval(() => void this.pollHistory(), POLL_MS);
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  protected async run(): Promise<void> {
    const request = await this.request();
    if (!request) {
      return;
    }
    this.error.set(null);
    this.detail.set(null);
    try {
      const run = await this.api.startBacktest(request);
      this.currentId = run.id;
      this.runStatus.set(run.status);
      this.clearRunTimer();
      this.runTimer = setInterval(() => void this.poll(), POLL_MS);
      void this.poll();
    } catch (error) {
      this.error.set(errorMessage(error));
      this.runStatus.set(null);
    }
  }

  protected async openRun(run: BacktestRunDto): Promise<void> {
    try {
      this.detail.set(await this.api.getBacktest(run.id));
      this.error.set(null);
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  protected openTrade(symbol: string, entry: string, exit: string): void {
    const params = new URLSearchParams();
    params.set('symbol', symbol);
    params.set('from', entry);
    params.set('to', exit);
    if (this.currentId) {
      params.set('bt', this.currentId);
    }
    void this.router.navigateByUrl(`/chart?${params.toString()}`);
  }

  // ------------------------------------------------------------ опитування

  protected historyProgress(): string {
    const job = this.historyJob();
    if (!job) {
      return '';
    }
    return `${job.status} · ${pct(job.progress, 0)} · ${job.message}`;
  }

  protected runProgressText(): string {
    const status = this.runStatus();
    if (status === null || status === 'done' || status === 'failed') {
      return '';
    }
    return status === 'queued' ? 'у черзі…' : 'виконується…';
  }

  protected totalTrades(): number {
    return this.detail()?.result?.trades.length ?? 0;
  }

  protected equity(): Array<{ date: string; equity: number; benchmark: number }> {
    return this.detail()?.result?.equity ?? [];
  }

  protected trades(): Array<{
    symbol: string;
    entry_date: string;
    exit_date: string;
    exit_price: number;
    qty: number;
    pnl: number;
    strategy: string;
  }> {
    return this.detail()?.result?.trades ?? [];
  }

  private async poll(): Promise<void> {
    const id = this.currentId;
    if (!id) {
      return;
    }
    try {
      const detail = await this.api.getBacktest(id);
      this.detail.set(detail);
      this.runStatus.set(detail.run.status);
      if (detail.run.status === 'done' || detail.run.status === 'failed') {
        this.clearRunTimer();
        if (detail.run.status === 'failed' && detail.run.error) {
          this.error.set(detail.run.error);
        }
        void this.reloadRuns();
      }
    } catch {
      // Тимчасова помилка опитування — спробуємо на наступному тіку.
    }
  }

  private async pollHistory(): Promise<void> {
    try {
      const job = await this.api.getHistoryJob();
      this.historyJob.set(job);
      if (job.status === 'done' || job.status === 'failed') {
        this.clearHistoryTimer();
      }
    } catch {
      // Тимчасова помилка опитування — спробуємо на наступному тіку.
    }
  }

  private async reloadRuns(): Promise<void> {
    try {
      this.runs.set(await this.api.listBacktests());
      this.loading.set(false);
    } catch (error) {
      this.error.set(errorMessage(error));
      this.loading.set(false);
    }
  }

  private clearRunTimer(): void {
    if (this.runTimer !== null) {
      clearInterval(this.runTimer);
      this.runTimer = null;
    }
  }

  private clearHistoryTimer(): void {
    if (this.historyTimer !== null) {
      clearInterval(this.historyTimer);
      this.historyTimer = null;
    }
  }

  private clearTimers(): void {
    this.clearRunTimer();
    this.clearHistoryTimer();
  }
}
