import { Component, DestroyRef, OnInit, computed, inject, signal, viewChild } from '@angular/core';
import { RouterLink } from '@angular/router';

import { errorMessage } from '../../core/api-error';
import { EngineService } from '../../core/engine.service';
import { EngineStore } from '../../core/engine-store';
import { EventsService } from '../../core/events.service';
import type { AccountDto } from '../../core/generated/AccountDto';
import type { JournalEventDto } from '../../core/generated/JournalEventDto';
import type { PositionDto } from '../../core/generated/PositionDto';
import { formatEtKyiv } from '../../core/tz';
import { UK } from '../../i18n/uk';
import { EtKyivTimePipe } from '../../shared/et-kyiv-time.pipe';
import { KillSwitchDialog } from '../../shared/kill-switch-dialog';
import { MoneyPipe } from '../../shared/money.pipe';

const FEED_LIMIT = 20;
const RELOAD_MS = 30_000;

/** Огляд: стан рушія, капітал, позиції, ліміти й стрічка подій. */
@Component({
  selector: 'app-overview',
  standalone: true,
  imports: [RouterLink, MoneyPipe, EtKyivTimePipe, KillSwitchDialog],
  templateUrl: './overview.html',
  styleUrl: './overview.scss',
})
export class Overview implements OnInit {
  private readonly api = inject(EngineService);
  private readonly events = inject(EventsService);
  private readonly destroyRef = inject(DestroyRef);
  protected readonly store = inject(EngineStore);

  private readonly killDialog = viewChild(KillSwitchDialog);

  protected readonly text = UK;
  protected readonly account = signal<AccountDto | null>(null);
  protected readonly positions = signal<PositionDto[]>([]);
  protected readonly feed = signal<JournalEventDto[]>([]);
  protected readonly error = signal<string | null>(null);
  protected readonly busy = signal(false);

  private readonly offs: Array<() => void> = [];
  private timer: ReturnType<typeof setInterval> | null = null;

  /** Ризик-ліміти з поточної конфігурації. */
  protected readonly risk = computed(() => this.store.config()?.risk ?? null);

  protected readonly modeText = computed(() => {
    switch (this.store.modeState()) {
      case 'running':
        return UK.engine.running;
      case 'paused':
        return UK.engine.paused;
      case 'killed':
        return UK.engine.killed;
      default:
        return UK.engine.unknown;
    }
  });

  protected readonly pauseLabel = computed(() =>
    this.store.modeState() === 'running' ? 'Пауза' : 'Відновити',
  );

  /** Σ(qty × current_price) / equity. */
  protected readonly exposure = computed(() => {
    const equity = Number(this.account()?.equity ?? 0);
    if (!(equity > 0)) {
      return 0;
    }
    const value = this.positions().reduce((sum, p) => sum + p.qty * Number(p.current_price), 0);
    return value / equity;
  });

  /** Денний результат як частка капіталу (від'ємна — збиток). */
  protected readonly dayPct = computed(() => {
    const equity = Number(this.account()?.equity ?? 0);
    if (!(equity > 0)) {
      return 0;
    }
    return Number(this.account()?.day_pnl ?? 0) / equity;
  });

  protected readonly exposureText = computed(() => `${(this.exposure() * 100).toFixed(1)}%`);

  protected readonly exposureLimitText = computed(() => {
    const limit = this.risk()?.max_gross_exposure;
    return limit === undefined ? '—' : `${(limit * 100).toFixed(0)}%`;
  });

  protected readonly positionsLimitText = computed(() => {
    const max = this.risk()?.max_open_positions;
    return max === undefined ? '—' : `${this.positions().length} / ${max}`;
  });

  protected readonly dailyText = computed(() => {
    const limit = this.risk()?.daily_loss_limit;
    const pct = `${(this.dayPct() * 100).toFixed(1)}%`;
    return limit === undefined ? pct : `${pct} / −${(limit * 100).toFixed(0)}%`;
  });

  protected readonly dailyBreached = computed(() => {
    const limit = this.risk()?.daily_loss_limit;
    return limit !== undefined && this.dayPct() <= -limit;
  });

  ngOnInit(): void {
    void this.reload();

    this.offs.push(
      this.events.on('journal', (event) => {
        if (event.type === 'journal') {
          this.pushEvent(event.event);
        }
      }),
    );
    for (const type of ['order_filled', 'stop_hit', 'status_changed'] as const) {
      this.offs.push(this.events.on(type, () => void this.reloadPositions()));
    }

    this.timer = setInterval(() => void this.reload(), RELOAD_MS);
    this.destroyRef.onDestroy(() => {
      for (const off of this.offs) {
        off();
      }
      if (this.timer !== null) {
        clearInterval(this.timer);
      }
    });
  }

  protected nextStepAt(at: string): string {
    return formatEtKyiv(at);
  }

  protected isLoss(value: string | null | undefined): boolean {
    return Number(value ?? 0) < 0;
  }

  protected openKill(): void {
    this.killDialog()?.open();
  }

  protected async togglePause(): Promise<void> {
    const state = this.store.modeState();
    if (this.busy() || state === 'unknown') {
      return;
    }
    if (state === 'killed') {
      const ok =
        typeof confirm === 'function'
          ? confirm('Рушій зупинено аварійно. Підтвердити відновлення роботи?')
          : true;
      if (!ok) {
        return;
      }
    }
    this.busy.set(true);
    this.error.set(null);
    try {
      if (state === 'running') {
        await this.api.pauseEngine();
      } else {
        await this.api.resumeEngine();
      }
      await this.store.refresh();
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.busy.set(false);
    }
  }

  private async reload(): Promise<void> {
    await Promise.allSettled([this.reloadPositions(), this.reloadAccount(), this.reloadFeed()]);
  }

  private async reloadAccount(): Promise<void> {
    try {
      this.account.set(await this.api.getAccount());
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  private async reloadPositions(): Promise<void> {
    try {
      this.positions.set(await this.api.getPositions());
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  private async reloadFeed(): Promise<void> {
    try {
      this.feed.set(await this.api.getJournal({ limit: FEED_LIMIT }));
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  /** Нова подія журналу — на початок стрічки, без дублікатів. */
  private pushEvent(event: JournalEventDto): void {
    this.feed.update((feed) => {
      if (feed.some((e) => e.id === event.id)) {
        return feed;
      }
      return [event, ...feed].slice(0, FEED_LIMIT);
    });
  }
}
