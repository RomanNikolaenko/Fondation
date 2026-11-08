import { Injectable, computed, inject, signal } from '@angular/core';

import { EngineService } from './engine.service';
import { EventsService } from './events.service';
import type { EngineConfig } from './generated/EngineConfig';
import type { IntentStatus } from './generated/IntentStatus';
import type { StatusDto } from './generated/StatusDto';

const PENDING: IntentStatus[] = ['pending_approval'];
const REFRESH_MS = 30_000;

/** Спільний стан рушія для оболонки й екранів. */
@Injectable({ providedIn: 'root' })
export class EngineStore {
  private readonly api = inject(EngineService);
  private readonly events = inject(EventsService);

  readonly status = signal<StatusDto | null>(null);
  readonly config = signal<EngineConfig | null>(null);
  readonly pendingCount = signal(0);
  readonly modeState = computed(() => this.status()?.engine_mode.state ?? 'unknown');

  private started = false;
  private timer: ReturnType<typeof setInterval> | null = null;

  start(): void {
    if (this.started) {
      return;
    }
    this.started = true;
    this.events.start();
    for (const type of ['status_changed', 'auto_paused', 'killed', 'intents_pending'] as const) {
      this.events.on(type, () => void this.refresh());
    }
    void this.refresh();
    this.timer = setInterval(() => void this.refresh(), REFRESH_MS);
  }

  stop(): void {
    if (this.timer !== null) {
      clearInterval(this.timer);
      this.timer = null;
    }
    this.started = false;
  }

  /** Оновити статус, конфігурацію й кількість намірів на схвалення. */
  async refresh(): Promise<void> {
    const [status, config, pending] = await Promise.allSettled([
      this.api.getStatus(),
      this.api.getConfig(),
      this.api.getIntents(PENDING),
    ]);
    if (status.status === 'fulfilled') {
      this.status.set(status.value);
    }
    if (config.status === 'fulfilled') {
      this.config.set(config.value);
    }
    if (pending.status === 'fulfilled') {
      this.pendingCount.set(pending.value.length);
    }
  }
}
