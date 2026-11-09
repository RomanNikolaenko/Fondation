import { Component, OnInit, inject, signal } from '@angular/core';

import { errorMessage } from '../../core/api-error';
import { EngineService } from '../../core/engine.service';
import type { ImportResultDto } from '../../core/generated/ImportResultDto';
import type { UniverseEntryDto } from '../../core/generated/UniverseEntryDto';

/** Секція «Список тікерів»: таблиця юніверсу, додавання, імпорт CSV. */
@Component({
  selector: 'app-universe-section',
  standalone: true,
  template: `
    <fieldset class="card">
      <legend>Список тікерів</legend>
      @if (error(); as message) {
        <p class="error" role="alert">{{ message }}</p>
      }
      @if (importResult(); as result) {
        <p class="muted">Імпорт: додано {{ result.added }} · пропущено {{ result.skipped }}</p>
      }
      <div class="add">
        <input
          type="text"
          name="new-symbol"
          autocomplete="off"
          placeholder="Наприклад, AAPL"
          [value]="newSymbol()"
          (input)="setNewSymbol($event)"
        />
        <button
          type="button"
          class="secondary"
          [disabled]="newSymbol().trim() === ''"
          (click)="add()"
        >
          Додати тікер
        </button>
        <label class="import">
          <input type="file" accept=".csv,text/csv" (change)="onFile($event)" />
          <span class="muted">імпорт CSV</span>
        </label>
      </div>

      @if (loading()) {
        <p class="muted">Завантаження…</p>
      } @else if (rows().length === 0) {
        <p class="muted">Юніверс порожній.</p>
      } @else {
        <table>
          <thead>
            <tr>
              <th>Символ</th>
              <th>Назва</th>
              <th>Сектор</th>
              <th>Джерело</th>
              <th>Виключено</th>
              <th>Дії</th>
            </tr>
          </thead>
          <tbody>
            @for (row of rows(); track row.symbol) {
              <tr>
                <td>{{ row.symbol }}</td>
                <td>{{ row.name }}</td>
                <td>{{ row.sector }}</td>
                <td>{{ row.source }}</td>
                <td>
                  <label class="excluded">
                    <input
                      type="checkbox"
                      name="{{ row.symbol }}-excluded"
                      [checked]="row.excluded"
                      (change)="toggleExcluded(row, $event)"
                    />
                  </label>
                </td>
                <td>
                  @if (row.source === 'manual') {
                    <button
                      type="button"
                      class="secondary"
                      data-fieldkey="remove"
                      (click)="remove(row)"
                    >
                      Видалити
                    </button>
                  }
                </td>
              </tr>
            }
          </tbody>
        </table>
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
    .add {
      display: flex;
      align-items: center;
      gap: 0.6rem;
      flex-wrap: wrap;
    }
    .import {
      display: flex;
      align-items: center;
      gap: 0.4rem;
    }
    input,
    button {
      font-size: 0.88rem;
    }
    input[type='text'] {
      width: 10rem;
      text-transform: uppercase;
      padding: 0.45rem 0.6rem;
      border: 1px solid var(--border);
      border-radius: 0.45rem;
      background: var(--surface);
      color: var(--text);
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
    .muted {
      margin: 0;
      color: var(--text-muted);
      font-size: 0.85rem;
    }
    table {
      width: 100%;
      border-collapse: collapse;
      font-size: 0.88rem;
    }
    th,
    td {
      padding: 0.4rem 0.6rem;
      text-align: left;
      border-bottom: 1px solid var(--border);
    }
    th {
      color: var(--text-muted);
      font-weight: 600;
      font-size: 0.78rem;
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
export class UniverseSection implements OnInit {
  private readonly api = inject(EngineService);

  protected readonly rows = signal<UniverseEntryDto[]>([]);
  protected readonly newSymbol = signal('');
  protected readonly importResult = signal<ImportResultDto | null>(null);
  protected readonly loading = signal(true);
  protected readonly error = signal<string | null>(null);

  ngOnInit(): void {
    void this.reload();
  }

  protected setNewSymbol(event: Event): void {
    this.newSymbol.set((event.target as HTMLInputElement).value);
  }

  protected async add(): Promise<void> {
    const symbol = this.newSymbol().trim().toUpperCase();
    if (symbol === '') {
      return;
    }
    this.error.set(null);
    this.importResult.set(null);
    try {
      await this.api.addSymbol(symbol);
      this.newSymbol.set('');
      await this.reload();
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  protected async toggleExcluded(row: UniverseEntryDto, event: Event): Promise<void> {
    const excluded = (event.target as HTMLInputElement).checked;
    this.error.set(null);
    try {
      await this.api.setExcluded(row.symbol, excluded);
      await this.reload();
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  protected async remove(row: UniverseEntryDto): Promise<void> {
    this.error.set(null);
    try {
      await this.api.removeSymbol(row.symbol);
      await this.reload();
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  protected async onFile(event: Event): Promise<void> {
    const inputEl = event.target as HTMLInputElement;
    const file = inputEl.files?.[0];
    if (!file) {
      return;
    }
    this.error.set(null);
    this.importResult.set(null);
    try {
      const text = await file.text();
      const result = await this.api.importUniverse(text);
      this.importResult.set(result);
      await this.reload();
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      inputEl.value = '';
    }
  }

  private async reload(): Promise<void> {
    this.loading.set(true);
    try {
      this.rows.set(await this.api.getUniverse());
      this.error.set(null);
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.loading.set(false);
    }
  }
}
