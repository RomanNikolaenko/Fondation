import {
  Component,
  DestroyRef,
  OnInit,
  inject,
  signal,
} from '@angular/core';

import { errorMessage } from '../../core/api-error';
import { EngineService } from '../../core/engine.service';
import { EventsService } from '../../core/events.service';
import type { JournalEventDto } from '../../core/generated/JournalEventDto';
import type { JournalQuery } from '../../core/engine.service';
import { todayEt } from '../../core/tz';
import { csvBlob, KyivDatePipe } from './csv';
import { JOURNAL_KINDS } from './kinds';

function inputValue(event: Event): string {
  return (event.target as HTMLInputElement).value;
}

/** Екран «Журнал»: події рушія, фільтри та експорт у CSV. */
@Component({
  selector: 'app-journal',
  standalone: true,
  imports: [KyivDatePipe],
  templateUrl: './journal.html',
  styleUrl: './journal.scss',
})
export class Journal implements OnInit {
  private readonly api = inject(EngineService);
  private readonly events = inject(EventsService);
  private readonly destroyRef = inject(DestroyRef);

  protected readonly kinds = JOURNAL_KINDS;
  protected readonly from = signal('');
  protected readonly to = signal(todayEt());
  protected readonly symbol = signal('');
  protected readonly kindsText = signal('');

  protected readonly eventsList = signal<JournalEventDto[]>([]);
  protected readonly selected = signal<Set<number>>(new Set());
  protected readonly loading = signal(true);
  protected readonly error = signal<string | null>(null);

  ngOnInit(): void {
    void this.reload();
    this.destroyRef.onDestroy(
      this.events.on('journal', () => void this.reload()),
    );
  }

  protected setFrom(event: Event): void {
    this.from.set(inputValue(event));
  }

  protected setTo(event: Event): void {
    this.to.set(inputValue(event));
  }

  protected setSymbol(event: Event): void {
    this.symbol.set(inputValue(event));
  }

  protected setKinds(event: Event): void {
    this.kindsText.set(inputValue(event));
  }

  protected onSubmit(event: Event): void {
    event.preventDefault();
    void this.reload();
  }

  /** Запит журналу, побудований з фільтрів (порожні поля не передаються). */
  protected buildQuery(): JournalQuery {
    const query: JournalQuery = {};
    if (this.from()) {
      query['from'] = this.from();
    }
    if (this.to()) {
      query['to'] = this.to();
    }
    if (this.symbol()) {
      query['symbol'] = this.symbol();
    }
    if (this.kindsText()) {
      query['kinds'] = this.kindsText();
    }
    return query;
  }

  protected toggle(id: number): void {
    this.selected.update((current) => {
      const next = new Set(current);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }

  protected data(row: JournalEventDto): string {
    return JSON.stringify(row.data, null, 2);
  }

  /** Завантаження CSV через `<a download>` (у WebView2 зберігає файл). */
  protected export(): void {
    const blob = csvBlob(this.eventsList());
    const url = URL.createObjectURL(blob);
    const name = `fond-journal-${todayEt().replaceAll('-', '')}.csv`;
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = name;
    document.body.append(anchor);
    anchor.click();
    anchor.remove();
    URL.revokeObjectURL(url);
  }

  private async reload(): Promise<void> {
    this.loading.set(true);
    try {
      this.eventsList.set(await this.api.getJournal(this.buildQuery()));
      this.selected.set(new Set());
      this.error.set(null);
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.loading.set(false);
    }
  }
}
