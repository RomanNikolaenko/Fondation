import { Injectable, signal } from '@angular/core';
import { listen } from '@tauri-apps/api/event';

import type { EngineEventDto } from './generated/EngineEventDto';

export type EngineEventType = EngineEventDto['type'];

/** Потік подій рушія з Tauri (подія `engine-event`). */
@Injectable({ providedIn: 'root' })
export class EventsService {
  /** Остання отримана подія. */
  readonly events = signal<EngineEventDto | null>(null);

  private started = false;

  /** Почати оновлювати `events` (ідемпотентно). */
  start(): void {
    if (this.started) {
      return;
    }
    this.started = true;
    void listen<EngineEventDto>('engine-event', (event) => {
      this.events.set(event.payload);
    }).catch(() => {
      this.started = false;
    });
  }

  /**
   * Підписка на конкретний тип події. Повертає функцію відписки;
   * безпечна до виклику, поки `listen` ще не завершився.
   */
  on(type: EngineEventType, callback: (event: EngineEventDto) => void): () => void {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void listen<EngineEventDto>('engine-event', (event) => {
      if (event.payload.type === type) {
        callback(event.payload);
      }
    })
      .then((off) => {
        if (cancelled) {
          off();
        } else {
          unlisten = off;
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
      unlisten = undefined;
    };
  }
}
