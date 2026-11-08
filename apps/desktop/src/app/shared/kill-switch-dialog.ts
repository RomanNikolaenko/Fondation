import { Component, DestroyRef, ElementRef, inject, signal, viewChild } from '@angular/core';
import { listen } from '@tauri-apps/api/event';

import { errorMessage } from '../core/api-error';
import { EngineService } from '../core/engine.service';
import { EngineStore } from '../core/engine-store';

type KillMode = 'cancel' | 'close';

/** Діалог аварійної зупинки; відкривається кнопкою або подією трею. */
@Component({
  selector: 'app-kill-switch-dialog',
  standalone: true,
  template: `
    <dialog #dialog class="kill" (cancel)="onCancel($event)">
      <h2>Аварійна зупинка</h2>
      <p class="lead">Нові ордери буде заборонено. Оберіть, що робити з наявними:</p>

      <label class="option">
        <input
          type="radio"
          name="kill-mode"
          value="cancel"
          [checked]="mode() === 'cancel'"
          (change)="mode.set('cancel')"
        />
        <span>
          <strong>Скасувати ордери, позиції залишити під стопами</strong>
          <small>Захисні стопи продовжують діяти.</small>
        </span>
      </label>

      <label class="option">
        <input
          type="radio"
          name="kill-mode"
          value="close"
          [checked]="mode() === 'close'"
          (change)="mode.set('close')"
        />
        <span>
          <strong>Скасувати все і закрити всі позиції</strong>
          <small>Продаж за ринком; результат може бути гіршим за поточну оцінку.</small>
        </span>
      </label>

      <label class="ack">
        <input type="checkbox" [checked]="acknowledged()" (change)="toggleAck($event)" />
        <span>Я розумію наслідки</span>
      </label>

      @if (error()) {
        <p class="error" role="alert">{{ error() }}</p>
      }

      <div class="actions">
        <button type="button" class="secondary" (click)="close()" [disabled]="busy()">
          Скасувати
        </button>
        <button
          type="button"
          class="danger"
          [disabled]="!acknowledged() || busy()"
          (click)="confirm()"
        >
          {{ busy() ? 'Зупиняємо…' : 'Зупинити' }}
        </button>
      </div>
    </dialog>
  `,
  styles: `
    dialog {
      width: min(560px, calc(100% - 2rem));
      padding: 1.5rem;
      border: 1px solid var(--border);
      border-radius: 0.8rem;
      background: var(--surface);
      color: var(--text);
    }
    dialog::backdrop {
      background: rgb(0 0 0 / 45%);
    }
    h2 {
      margin: 0 0 0.5rem;
    }
    .lead {
      margin: 0 0 1rem;
      color: var(--text-muted);
    }
    .option,
    .ack {
      display: flex;
      align-items: flex-start;
      gap: 0.6rem;
      margin-bottom: 0.9rem;
      cursor: pointer;
    }
    .option span {
      display: flex;
      flex-direction: column;
      gap: 0.15rem;
    }
    small {
      color: var(--text-muted);
    }
    .error {
      color: #b91c1c;
    }
    .actions {
      display: flex;
      justify-content: flex-end;
      gap: 0.6rem;
    }
    button {
      padding: 0.55rem 1rem;
      border: 1px solid transparent;
      border-radius: 0.5rem;
      cursor: pointer;
    }
    .secondary {
      border-color: var(--border);
      background: var(--surface-2);
      color: var(--text);
    }
    .danger {
      background: #b91c1c;
      color: #fff;
    }
    .danger:disabled {
      cursor: not-allowed;
      opacity: 0.45;
    }
  `,
})
export class KillSwitchDialog {
  private readonly api = inject(EngineService);
  private readonly store = inject(EngineStore);
  private readonly dialog = viewChild<ElementRef<HTMLDialogElement>>('dialog');
  private readonly destroyRef = inject(DestroyRef);

  protected readonly mode = signal<KillMode>('cancel');
  protected readonly acknowledged = signal(false);
  protected readonly busy = signal(false);
  protected readonly error = signal<string | null>(null);

  private unlisten: (() => void) | undefined;

  constructor() {
    void listen('open-kill-dialog', () => this.open())
      .then((off) => {
        this.unlisten = off;
      })
      .catch(() => {});
    this.destroyRef.onDestroy(() => this.unlisten?.());
  }

  /** Відкрити модально (викликається з огляду або трею). */
  open(): void {
    this.error.set(null);
    const dialog = this.dialog()?.nativeElement;
    if (!dialog) {
      return;
    }
    if (typeof dialog.showModal === 'function') {
      dialog.showModal();
    } else {
      dialog.setAttribute('open', '');
    }
  }

  protected close(): void {
    this.dialog()?.nativeElement?.close?.();
  }

  protected onCancel(event: Event): void {
    if (this.busy()) {
      event.preventDefault();
    }
  }

  protected toggleAck(event: Event): void {
    this.acknowledged.set((event.target as HTMLInputElement).checked);
  }

  protected async confirm(): Promise<void> {
    if (!this.acknowledged() || this.busy()) {
      return;
    }
    this.busy.set(true);
    this.error.set(null);
    try {
      await this.api.killEngine(this.mode() === 'close');
      this.close();
      await this.store.refresh();
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.busy.set(false);
    }
  }
}
