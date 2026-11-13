import { Component, DestroyRef, OnInit, computed, inject, signal } from '@angular/core';

import { errorMessage } from '../../core/api-error';
import { EngineService } from '../../core/engine.service';
import { EventsService } from '../../core/events.service';
import type { AccountDto } from '../../core/generated/AccountDto';
import type { EngineConfig } from '../../core/generated/EngineConfig';
import type { PositionDto } from '../../core/generated/PositionDto';
import { MoneyPipe } from '../../shared/money.pipe';

/** Рядок редактора цілей: тікер і вага у відсотках (рядок вводу). */
interface TargetDraft {
  ticker: string;
  pct: string;
}

/** Рядок таблиці «ціль / факт»: ваги в пп, дельта в доларах. */
interface PortfolioRow {
  symbol: string;
  targetPct: number;
  weightPct: number;
  deviationPct: number;
  deltaUsd: number;
  qty: number;
  /** Дельта перевищує поріг рушія — ребаланс створить для неї намір. */
  over: boolean;
}

/** Стратегія портфеля в конфігурації рушія. */
const PORTFOLIO = 'portfolio';

function inputValue(event: Event): string {
  return (event.target as HTMLInputElement).value;
}

/** Екран «Портфель»: цільові ваги, поточні ваги й ручний ребаланс. */
@Component({
  selector: 'app-portfolio',
  standalone: true,
  imports: [MoneyPipe],
  templateUrl: './portfolio.html',
  styleUrl: './portfolio.scss',
})
export class Portfolio implements OnInit {
  private readonly api = inject(EngineService);
  private readonly events = inject(EventsService);
  private readonly destroyRef = inject(DestroyRef);

  protected readonly config = signal<EngineConfig | null>(null);
  protected readonly positions = signal<PositionDto[]>([]);
  protected readonly account = signal<AccountDto | null>(null);
  protected readonly error = signal<string | null>(null);
  protected readonly loading = signal(true);

  /** Чернетка цілей (вага у відсотках рядком — як введено). */
  protected readonly draft = signal<TargetDraft[]>([]);
  protected readonly newTicker = signal('');
  protected readonly newPct = signal('');
  protected readonly saving = signal(false);
  protected readonly savedMessage = signal<string | null>(null);

  protected readonly running = signal(false);
  protected readonly runMessage = signal<string | null>(null);

  /** Equity рахунку для ваг; 0 — рахунок ще не завантажено. */
  private readonly equity = computed(() => Number(this.account()?.equity ?? '0'));

  protected readonly enabled = computed(() => this.config()?.portfolio.enabled ?? false);

  /** Конфігурацію завантажено, а портфель вимкнено — ручний запуск неможливий. */
  protected readonly disabledInConfig = computed(() => this.config() !== null && !this.enabled());

  /** Сума ваг цілей у відсотках. */
  protected readonly totalPct = computed(() =>
    this.draft().reduce((sum, t) => sum + (Number(t.pct) || 0), 0),
  );

  protected readonly overLimit = computed(() => this.totalPct() > 100 + 1e-9);

  /**
   * Поріг дельти в доларах, як у рушії: `max(drift × equity, min_order_usd)`.
   * `null` — конфігурацію ще не завантажено.
   */
  private readonly thresholdUsd = computed(() => {
    const portfolio = this.config()?.portfolio;
    if (!portfolio) {
      return null;
    }
    return Math.max(portfolio.drift * this.equity(), portfolio.min_order_usd);
  });

  /** Рядки таблиці: цілі чернетки + позиції портфеля поза цілями (ціль 0%). */
  protected readonly rows = computed<PortfolioRow[]>(() => {
    const equity = this.equity();
    const threshold = this.thresholdUsd();
    const value = new Map<string, { value: number; qty: number }>();
    for (const p of this.positions()) {
      if (p.strategy !== PORTFOLIO) {
        continue;
      }
      value.set(p.symbol, { value: p.qty * Number(p.current_price), qty: p.qty });
    }
    const row = (symbol: string, targetPct: number): PortfolioRow => {
      const fact = value.get(symbol) ?? { value: 0, qty: 0 };
      const weightPct = equity > 0 ? (fact.value / equity) * 100 : 0;
      const deltaUsd = (targetPct / 100) * equity - fact.value;
      return {
        symbol,
        targetPct,
        weightPct,
        deviationPct: weightPct - targetPct,
        deltaUsd,
        qty: fact.qty,
        over: threshold !== null && Math.abs(deltaUsd) >= threshold,
      };
    };
    const rows = this.draft().map((t) => row(t.ticker, Number(t.pct) || 0));
    const seen = new Set(rows.map((r) => r.symbol));
    for (const symbol of value.keys()) {
      if (!seen.has(symbol)) {
        rows.push(row(symbol, 0));
      }
    }
    return rows.sort((a, b) => a.symbol.localeCompare(b.symbol));
  });

  ngOnInit(): void {
    void this.loadAll();
    // Живе оновлення факту: події журналу — створення намірів, відправка, звірка.
    this.destroyRef.onDestroy(this.events.on('journal', () => void this.refreshFacts()));
    this.destroyRef.onDestroy(this.events.on('order_filled', () => void this.refreshFacts()));
  }

  protected pct(value: number): string {
    return `${value.toFixed(1)}%`;
  }

  protected onNewTicker(event: Event): void {
    this.newTicker.set(inputValue(event));
  }

  protected onNewPct(event: Event): void {
    this.newPct.set(inputValue(event));
  }

  /** Нова вага наявної цілі: чернетка оновлюється цілком, щоб сума й рядки перерахувались. */
  protected setPct(ticker: string, event: Event): void {
    const pct = inputValue(event);
    this.savedMessage.set(null);
    this.draft.update((list) => list.map((t) => (t.ticker === ticker ? { ...t, pct } : t)));
  }

  /** Додає тікер із вагою; помилку вводу показує в `error`. */
  protected addTarget(): void {
    const ticker = this.newTicker().trim().toUpperCase();
    const raw = this.newPct().trim();
    const pct = Number(raw);
    if (ticker === '') {
      this.error.set('Вкажіть тікер.');
      return;
    }
    if (raw === '' || !Number.isFinite(pct) || pct <= 0 || pct > 100) {
      this.error.set('Вага має бути від 0 до 100 %.');
      return;
    }
    if (this.draft().some((t) => t.ticker === ticker)) {
      this.error.set(`Тікер ${ticker} уже в цілях.`);
      return;
    }
    this.error.set(null);
    this.savedMessage.set(null);
    this.draft.update((list) => [...list, { ticker, pct: raw }]);
    this.newTicker.set('');
    this.newPct.set('');
  }

  protected removeTarget(ticker: string): void {
    this.error.set(null);
    this.savedMessage.set(null);
    this.draft.update((list) => list.filter((t) => t.ticker !== ticker));
  }

  /** Зберігає цілі в конфігурації (putConfig); ваги переводяться у частки. */
  protected async saveTargets(): Promise<void> {
    const config = this.config();
    if (config === null || this.saving()) {
      return;
    }
    if (
      this.draft().some((t) => {
        const pct = Number(t.pct);
        return t.pct.trim() === '' || !Number.isFinite(pct) || pct <= 0 || pct > 100;
      })
    ) {
      this.error.set('Виправте ваги: кожна має бути від 0 до 100 %.');
      return;
    }
    this.saving.set(true);
    this.savedMessage.set(null);
    this.error.set(null);
    try {
      const next = structuredClone(config);
      next.portfolio.targets = Object.fromEntries(
        this.draft().map((t) => [t.ticker, Math.round((Number(t.pct) / 100) * 1e9) / 1e9]),
      );
      const saved = await this.api.putConfig(next);
      this.config.set(saved);
      this.syncDraft();
      this.savedMessage.set('Цілі збережено.');
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.saving.set(false);
    }
  }

  /** Ручний ребаланс: 202 — запуск у фоні, 409 — текстом помилки. */
  protected async run(): Promise<void> {
    if (this.running()) {
      return;
    }
    this.running.set(true);
    this.runMessage.set(null);
    this.error.set(null);
    try {
      const queued = await this.api.runPortfolioRebalance();
      this.runMessage.set(`Ребаланс запущено у фоні — наміри з’являться на сесію ${queued.date}.`);
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.running.set(false);
    }
  }

  private async loadAll(): Promise<void> {
    await this.refreshFacts();
    try {
      const config = await this.api.getConfig();
      this.config.set(config);
      this.syncDraft();
      this.error.set(null);
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.loading.set(false);
    }
  }

  private async refreshFacts(): Promise<void> {
    try {
      const [positions, account] = await Promise.all([
        this.api.getPositions(),
        this.api.getAccount(),
      ]);
      this.positions.set(positions);
      this.account.set(account);
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  /** Чернетка цілей із конфігурації (ваги ×100, без нульових). */
  private syncDraft(): void {
    const targets = Object.entries(this.config()?.portfolio.targets ?? {}).filter(
      ([, weight]) => weight > 0,
    );
    this.draft.set(
      targets
        .map(([ticker, weight]) => ({ ticker, pct: `${+(weight * 100).toFixed(6)}` }))
        .sort((a, b) => a.ticker.localeCompare(b.ticker)),
    );
  }
}
