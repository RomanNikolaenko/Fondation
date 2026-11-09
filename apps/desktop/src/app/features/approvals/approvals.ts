import { Component, DestroyRef, OnInit, inject, signal } from '@angular/core';

import { errorMessage } from '../../core/api-error';
import { EngineService } from '../../core/engine.service';
import { EngineStore } from '../../core/engine-store';
import { EventsService } from '../../core/events.service';
import type { Intent } from '../../core/generated/Intent';
import { etWallTimeToUtc } from '../../core/tz';
import { ApprovalCard } from './approval-card';

/** Дедлайн схвалення для сесії: момент UTC. */
export function deadlineFor(sessionDate: string, deadlineEt: string): Date {
  return etWallTimeToUtc(sessionDate, deadlineEt);
}

/** Екран «Схвалення»: наміри, що чекають на підтвердження. */
@Component({
  selector: 'app-approvals',
  standalone: true,
  imports: [ApprovalCard],
  templateUrl: './approvals.html',
  styleUrl: './approvals.scss',
})
export class Approvals implements OnInit {
  private readonly api = inject(EngineService);
  private readonly events = inject(EventsService);
  private readonly destroyRef = inject(DestroyRef);
  protected readonly store = inject(EngineStore);

  protected readonly intents = signal<Intent[]>([]);
  protected readonly equity = signal<string | null>(null);
  protected readonly loading = signal(true);
  protected readonly error = signal<string | null>(null);

  constructor() {
    this.destroyRef.onDestroy(this.events.on('intents_pending', () => void this.reload()));
  }

  ngOnInit(): void {
    void this.reload();
  }

  protected deadline(): string | null {
    const schedule = this.store.config()?.schedule;
    if (!schedule) {
      return null;
    }
    return schedule.approval_deadline;
  }

  protected deadlineAt(intent: Intent): string | null {
    const deadline = this.deadline();
    if (!deadline) {
      return null;
    }
    const at = deadlineFor(intent.session_date, deadline);
    return Number.isNaN(at.getTime()) ? null : at.toISOString();
  }

  protected async onApprove(id: string): Promise<void> {
    this.error.set(null);
    try {
      await this.api.approveIntent(id);
      this.intents.update((list) => list.filter((intent) => intent.id !== id));
      await this.store.refresh();
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  protected async onReject(id: string): Promise<void> {
    this.error.set(null);
    try {
      await this.api.rejectIntent(id);
      this.intents.update((list) => list.filter((intent) => intent.id !== id));
      await this.store.refresh();
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  private async reload(): Promise<void> {
    this.loading.set(true);
    try {
      const [intents, account] = await Promise.all([
        this.api.getIntents(['pending_approval']),
        this.api.getAccount().catch(() => null),
      ]);
      this.intents.set(intents);
      this.equity.set(account?.equity ?? null);
      this.error.set(null);
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.loading.set(false);
    }
  }
}
