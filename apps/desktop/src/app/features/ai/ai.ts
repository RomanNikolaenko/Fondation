import { Component, DestroyRef, OnInit, computed, inject, signal } from '@angular/core';

import { errorMessage } from '../../core/api-error';
import { EngineStore } from '../../core/engine-store';
import { EngineService } from '../../core/engine.service';
import { EventsService } from '../../core/events.service';
import type { AiProposalDto } from '../../core/generated/AiProposalDto';

/** Фільтр статусу пропозицій (значення — як у `AiProposalDto.status`). */
export type ProposalFilter = 'all' | 'recorded' | 'risk_rejected' | 'intent_created';

/** Мітки статусів українською. */
export function statusLabel(status: string): string {
  switch (status) {
    case 'recorded':
      return 'записано';
    case 'risk_rejected':
      return 'відхилено ризиком';
    case 'intent_created':
      return 'намір створено';
    default:
      return status;
  }
}

/** Мітка дії українською. */
export function actionLabel(action: string): string {
  return action === 'buy' ? 'купівля' : action === 'exit' ? 'вихід' : action;
}

function inputValue(event: Event): string {
  return (event.target as HTMLInputElement).value;
}

/** Екран «AI-аналіз»: журнал пропозицій Claude і кнопка ручного запуску аналізу. */
@Component({
  selector: 'app-ai',
  standalone: true,
  templateUrl: './ai.html',
  styleUrl: './ai.scss',
})
export class Ai implements OnInit {
  private readonly api = inject(EngineService);
  private readonly events = inject(EventsService);
  private readonly destroyRef = inject(DestroyRef);
  protected readonly store = inject(EngineStore);

  protected readonly proposals = signal<AiProposalDto[]>([]);
  protected readonly loading = signal(true);
  protected readonly error = signal<string | null>(null);
  protected readonly running = signal(false);
  /** Підсумок ручного запуску (дата аналізу). */
  protected readonly queued = signal<string | null>(null);

  protected readonly filter = signal<ProposalFilter>('all');
  protected readonly days = signal(7);

  protected readonly filtered = computed(() => {
    const filter = this.filter();
    const rows = this.proposals();
    if (filter === 'all') {
      return rows;
    }
    return rows.filter((row) => row.status === filter);
  });

  /** Зведення за статусами для рядка під заголовком. */
  protected readonly summary = computed(() => {
    const rows = this.proposals();
    const count = (status: string): number => rows.filter((row) => row.status === status).length;
    return {
      total: rows.length,
      recorded: count('recorded'),
      rejected: count('risk_rejected'),
      intents: count('intent_created'),
    };
  });

  /** Загальні витрати LLM за вікно (USD). */
  protected readonly totalCost = computed(() =>
    this.proposals().reduce((sum, row) => sum + row.cost_usd, 0),
  );

  ngOnInit(): void {
    void this.reload();
    // Аналіз у фоні пише події в журнал — оновлюємо список живо.
    this.destroyRef.onDestroy(this.events.on('journal', () => void this.reload()));
  }

  protected statusLabel(status: string): string {
    return statusLabel(status);
  }

  protected actionLabel(action: string): string {
    return actionLabel(action);
  }

  protected setFilter(event: Event): void {
    const value = inputValue(event);
    if (
      value === 'all' ||
      value === 'recorded' ||
      value === 'risk_rejected' ||
      value === 'intent_created'
    ) {
      this.filter.set(value);
    }
  }

  protected setDays(event: Event): void {
    const value = Number(inputValue(event));
    if (Number.isInteger(value) && value >= 1 && value <= 30) {
      this.days.set(value);
      void this.reload();
    }
  }

  /**
   * Ручний запуск вечірнього аналізу. В режимі автопідтвердження — із застереженням:
   * наміри з AI-пропозицій відправляться на біржу без ручного схвалення.
   */
  protected async run(): Promise<void> {
    if (this.running()) {
      return;
    }
    const auto = this.store.config()?.execution_mode === 'auto';
    if (
      auto &&
      !window.confirm(
        'Рушій працює в режимі автопідтвердження: наміри з AI-пропозицій будуть ' +
          'відправлені на біржу автоматично. Запустити аналіз?',
      )
    ) {
      return;
    }
    this.running.set(true);
    this.queued.set(null);
    try {
      const result = await this.api.runAiAnalysis();
      this.queued.set(result.date);
      await this.reload();
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.running.set(false);
    }
  }

  private async reload(): Promise<void> {
    this.loading.set(true);
    try {
      this.proposals.set(await this.api.getAiProposals(this.days()));
      this.error.set(null);
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.loading.set(false);
    }
  }
}
