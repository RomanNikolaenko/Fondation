import { Component, OnInit, inject, signal } from '@angular/core';

import { errorMessage } from '../../core/api-error';
import { EngineService } from '../../core/engine.service';
import type { SecretStatusDto } from '../../core/generated/SecretStatusDto';
import type { VerifyResultDto } from '../../core/generated/VerifyResultDto';

/** Секретні ключі: інтерфейс ніколи не показує збережені значення. */
export const SECRET_KEYS: ReadonlyArray<{ key: string; label: string }> = [
  { key: 'alpaca_key_id', label: 'Alpaca — ключ' },
  { key: 'alpaca_secret', label: 'Alpaca — секрет' },
  { key: 'anthropic_api_key', label: 'Anthropic — ключ' },
  { key: 'finnhub_api_key', label: 'Finnhub — ключ' },
];

type Presence = Record<string, boolean>;

/** Секція «Ключі»: поля-паролі, бейджі наявності, перевірка доступу. */
@Component({
  selector: 'app-keys-section',
  standalone: true,
  template: `
    <fieldset class="card">
      <legend>Ключі</legend>
      <p class="hint">
        Збережені значення не показуються — лише стан «збережено / немає».
      </p>
      @for (field of keys; track field.key) {
        <div class="row">
          <label>
            <span>{{ field.label }}</span>
            <input
              type="password"
              name="{{ field.key }}"
              autocomplete="new-password"
              [value]="entry(field.key)"
              (input)="setEntry(field.key, $event)"
            />
          </label>
          <span class="badge" [class.absent]="!present()[field.key]">
            {{ present()[field.key] ? 'збережено' : 'немає' }}
          </span>
          <button
            type="button"
            class="secondary"
            [attr.data-fieldkey]="field.key"            [disabled]="!(entry(field.key).length > 0)"
            (click)="saveSecret(field.key)"
          >
            Зберегти
          </button>
        </div>
      }
      <div class="verify">
        <button type="button" class="secondary" (click)="runVerify()">
          Перевірити
        </button>
        @if (verifying()) {
          <span class="muted">Перевірка триває…</span>
        }
      </div>
      @if (verify(); as v) {
        <ul class="results">
          <li
            [class.fail]="!v.alpaca.ok"
          >Alpaca: {{ v.alpaca.ok ? 'ok' : v.alpaca.message ?? 'помилка' }}</li>
          <li
            [class.fail]="!v.finnhub.ok"
          >Finnhub: {{ v.finnhub.ok ? 'ok' : v.finnhub.message ?? 'помилка' }}</li>
          <li
            [class.fail]="!v.anthropic.ok"
          >Anthropic: {{ v.anthropic.ok ? 'ok' : v.anthropic.message ?? 'помилка' }}</li>
        </ul>
      }
      @if (error(); as message) {
        <p class="error" role="alert">{{ message }}</p>
      }
    </fieldset>
  `,
  styles: `
    .card {
      display: flex;
      flex-direction: column;
      gap: 0.6rem;
      padding: 1rem 1.1rem;
      border: 1px solid var(--border);
      border-radius: 0.75rem;
      background: var(--surface);
    }
    legend {
      font-weight: 600;
    }
    .hint {
      margin: 0;
      font-size: 0.8rem;
      color: var(--text-muted);
    }
    .row {
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto auto;
      gap: 0.6rem;
      align-items: end;
    }
    label {
      display: flex;
      flex-direction: column;
      gap: 0.25rem;
      font-size: 0.8rem;
      color: var(--text-muted);
    }
    input {
      padding: 0.45rem 0.6rem;
      border: 1px solid var(--border);
      border-radius: 0.45rem;
      background: var(--surface);
      color: var(--text);
    }
    .badge {
      padding: 0.2rem 0.6rem;
      border-radius: 999px;
      background: #dcfce7;
      color: #166534;
      font-size: 0.78rem;
    }
    .badge.absent {
      background: var(--surface-2);
      color: var(--text-muted);
    }
    button {
      padding: 0.45rem 0.9rem;
      border: 1px solid var(--border);
      border-radius: 0.45rem;
      background: var(--surface-2);
      color: var(--text);
      cursor: pointer;
    }
    button:disabled {
      cursor: not-allowed;
      opacity: 0.55;
    }
    .verify {
      display: flex;
      align-items: center;
      gap: 0.6rem;
    }
    .results {
      margin: 0;
      padding-left: 1.1rem;
      font-size: 0.88rem;
    }
    .fail {
      color: #b91c1c;
    }
    .muted {
      color: var(--text-muted);
      font-size: 0.85rem;
    }
    .error {
      margin: 0;
      padding: 0.6rem 0.8rem;
      border-radius: 0.5rem;
      background: #fee2e2;
      color: #991b1b;
    }
  `,
})
export class KeysSection implements OnInit {
  protected readonly keys = SECRET_KEYS;

  private readonly api = inject(EngineService);

  protected readonly present = signal<Presence>({});
  protected readonly entered = signal<Record<string, string>>({});
  protected readonly verify = signal<VerifyResultDto | null>(null);
  protected readonly verifying = signal(false);
  protected readonly error = signal<string | null>(null);

  ngOnInit(): void {
    void this.reload();
  }

  protected entry(key: string): string {
    return this.entered()[key] ?? '';
  }

  protected setEntry(key: string, event: Event): void {
    const value = (event.target as HTMLInputElement).value;
    this.entered.update((state) => ({ ...state, [key]: value }));
  }

  protected async saveSecret(key: string): Promise<void> {
    const value = this.entry(key);
    if (value === '') {
      return;
    }
    this.error.set(null);
    try {
      await this.api.setSecret(key, value);
      this.present.update((state) => ({ ...state, [key]: true }));
      this.entered.update((state) => ({ ...state, [key]: '' }));
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  protected async runVerify(): Promise<void> {
    this.error.set(null);
    this.verifying.set(true);
    try {
      this.verify.set(await this.api.verifySecrets());
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.verifying.set(false);
    }
  }

  private async reload(): Promise<void> {
    try {
      const secrets: SecretStatusDto[] = await this.api.getSecrets();
      const state: Presence = {};
      for (const secret of secrets) {
        state[secret.key] = secret.present;
      }
      this.present.set(state);
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }
}
