import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { EngineStore } from '../../core/engine-store';
import { EngineService } from '../../core/engine.service';
import { EventsService } from '../../core/events.service';
import type { AiProposalDto } from '../../core/generated/AiProposalDto';
import { Ai, actionLabel, statusLabel } from './ai';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

const BUY: AiProposalDto = {
  id: 1,
  at: '2026-11-10T21:30:00Z',
  run_date: '2026-11-10',
  proposal: {
    symbol: 'MSFT',
    action: 'buy',
    reference_price: 400,
    stop_price: 385,
    take_profit: 440,
    strength: 0.72,
    explanation: 'Тренд нагору, новини позитивні.',
    key_reasons: ['Ціна над опорою.'],
  },
  model: 'claude-haiku-5-5-20261001',
  input_tokens: 2200,
  output_tokens: 90,
  cost_usd: 0.01,
  status: 'recorded',
  reject_reason: null,
};

const REJECTED: AiProposalDto = {
  ...BUY,
  id: 2,
  proposal: { ...BUY.proposal, symbol: 'NVDA', action: 'exit' },
  status: 'risk_rejected',
  reject_reason: 'грос-ліміт',
};

describe('Ai labels', () => {
  it('status and action labels are Ukrainian', () => {
    expect(statusLabel('recorded')).toBe('записано');
    expect(statusLabel('risk_rejected')).toBe('відхилено ризиком');
    expect(statusLabel('intent_created')).toBe('намір створено');
    expect(actionLabel('buy')).toBe('купівля');
    expect(actionLabel('exit')).toBe('вихід');
  });
});

describe('Ai screen', () => {
  afterEach(() => TestBed.resetTestingModule());

  function configure(api: Partial<EngineService>, executionMode: 'approval' | 'auto'): void {
    const store = {
      status: signal(null),
      config: signal({ execution_mode: executionMode }),
      pendingCount: signal(0),
      modeState: signal('running'),
      start: vi.fn(),
      stop: vi.fn(),
      refresh: vi.fn().mockResolvedValue(undefined),
    };
    TestBed.configureTestingModule({
      imports: [Ai],
      providers: [
        { provide: EngineService, useValue: api },
        { provide: EngineStore, useValue: store },
        {
          provide: EventsService,
          useValue: { events: signal(null), start: vi.fn(), on: vi.fn(() => () => {}) },
        },
      ],
    });
  }

  it('shows proposals table with Ukrainian labels', async () => {
    const api = { getAiProposals: vi.fn().mockResolvedValue([BUY, REJECTED]) };
    configure(api, 'approval');

    const fixture = TestBed.createComponent(Ai);
    fixture.detectChanges();

    await vi.waitFor(() => {
      const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
      expect(text).toContain('MSFT');
      expect(text).toContain('купівля');
      expect(text).toContain('записано');
      expect(text).toContain('відхилено ризиком');
      expect(text).toContain('грос-ліміт');
      expect(text).toContain('claude-haiku-5-5-20261001');
    });
    expect(api.getAiProposals).toHaveBeenCalledWith(7);
  });

  it('status filter narrows the list', async () => {
    const api = { getAiProposals: vi.fn().mockResolvedValue([BUY, REJECTED]) };
    configure(api, 'approval');

    const fixture = TestBed.createComponent(Ai);
    fixture.detectChanges();
    await vi.waitFor(() => {
      expect((fixture.nativeElement as HTMLElement).textContent ?? '').toContain('MSFT');
    });

    const select = fixture.nativeElement.querySelector('select') as HTMLSelectElement;
    select.value = 'risk_rejected';
    select.dispatchEvent(new Event('input'));

    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).not.toContain('MSFT');
    expect(text).toContain('NVDA');
  });

  it('run button queues analysis and shows the analysis date', async () => {
    const api = {
      getAiProposals: vi.fn().mockResolvedValue([]),
      runAiAnalysis: vi.fn().mockResolvedValue({ date: '2026-11-10' }),
    };
    configure(api, 'approval');

    const fixture = TestBed.createComponent(Ai);
    fixture.detectChanges();

    const button = fixture.nativeElement.querySelector('button') as HTMLButtonElement;
    button.click();
    await vi.waitFor(() => {
      const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
      expect(text).toContain('Аналіз за 2026-11-10 запущено у фоні');
    });
    expect(api.runAiAnalysis).toHaveBeenCalledTimes(1);
  });

  it('run in auto mode asks for confirmation and aborts on decline', async () => {
    const api = {
      getAiProposals: vi.fn().mockResolvedValue([]),
      runAiAnalysis: vi.fn(),
    };
    configure(api, 'auto');
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);

    const fixture = TestBed.createComponent(Ai);
    fixture.detectChanges();
    const button = fixture.nativeElement.querySelector('button') as HTMLButtonElement;
    button.click();
    await vi.waitFor(() => expect(confirm).toHaveBeenCalled());

    expect(api.runAiAnalysis).not.toHaveBeenCalled();
    confirm.mockRestore();
  });

  it('api error is shown in Ukrainian', async () => {
    const api = {
      getAiProposals: vi.fn().mockRejectedValue({
        kind: 'http',
        status: 409,
        message: 'AI-аналіз уже виконується',
        details: [],
      }),
    };
    configure(api, 'approval');

    const fixture = TestBed.createComponent(Ai);
    fixture.detectChanges();

    await vi.waitFor(() => {
      const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
      expect(text).toContain('Помилка 409');
      expect(text).toContain('уже виконується');
    });
  });
});
