import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { EngineService } from '../../core/engine.service';
import { EventsService } from '../../core/events.service';
import type { JournalEventDto } from '../../core/generated/JournalEventDto';
import { Journal } from './journal';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

function event(id: number, at: string): JournalEventDto {
  return {
    id,
    at,
    kind: 'fill',
    symbol: 'AAPL',
    intent_id: null,
    message: `Подія ${id}`,
    data: { side: 'buy' },
  };
}

describe('Journal', () => {
  afterEach(() => TestBed.resetTestingModule());

  it('journal builds query from filters', async () => {
    const api = {
      getJournal: vi.fn().mockResolvedValue([event(1, '2026-11-09T14:30:00Z')]),
    };
    TestBed.configureTestingModule({
      imports: [Journal],
      providers: [
        { provide: EngineService, useValue: api },
        {
          provide: EventsService,
          useValue: { events: signal(null), start: vi.fn(), on: vi.fn(() => () => {}) },
        },
      ],
    });

    const fixture = TestBed.createComponent(Journal);
    fixture.detectChanges();

    // Початкове завантаження (без фільтрів, крім "до" = сьогодні ET).
    await vi.waitFor(() => expect(api.getJournal).toHaveBeenCalled());
    const initial = api.getJournal.mock.calls[0]?.[0] as Record<string, unknown>;
    expect(initial['to']).toMatch(/^\d{4}-\d{2}-\d{2}$/);
    expect(initial['from']).toBeUndefined();

    // Фільтри: символ і типи, потім «Застосувати».
    const symbol = fixture.nativeElement.querySelector(
      'input[name="symbol"]',
    ) as HTMLInputElement;
    symbol.value = 'MSFT';
    symbol.dispatchEvent(new Event('input'));

    const kinds = fixture.nativeElement.querySelector(
      'input[name="kinds"]',
    ) as HTMLInputElement;
    kinds.value = 'fill,order';
    kinds.dispatchEvent(new Event('input'));

    const form = (fixture.nativeElement as HTMLElement).querySelector(
      'form',
    ) as HTMLFormElement;
    form.dispatchEvent(new Event('submit', { cancelable: true }));

    await vi.waitFor(() => expect(api.getJournal).toHaveBeenCalledTimes(2));
    const query = api.getJournal.mock.calls[1]?.[0] as Record<string, unknown>;
    expect(query).toEqual({
      symbol: 'MSFT',
      kinds: 'fill,order',
      to: initial['to'],
    });
  });
});
