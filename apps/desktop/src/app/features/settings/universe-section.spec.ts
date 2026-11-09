import { TestBed } from '@angular/core/testing';

import { EngineService } from '../../core/engine.service';
import { UniverseSection } from './universe-section';

type MockFn = ReturnType<typeof vi.fn>;

interface ApiMock {
  getUniverse: MockFn;
  addSymbol: MockFn;
  setExcluded: MockFn;
  removeSymbol: MockFn;
  importUniverse: MockFn;
}

function apiMock(): ApiMock {
  return {
    getUniverse: vi.fn().mockResolvedValue([]),
    addSymbol: vi.fn(),
    setExcluded: vi.fn(),
    removeSymbol: vi.fn(),
    importUniverse: vi.fn(),
  };
}

function configure(api: ApiMock): void {
  TestBed.configureTestingModule({
    imports: [UniverseSection],
    providers: [{ provide: EngineService, useValue: api }],
  });
}

describe('UniverseSection', () => {
  afterEach(() => TestBed.resetTestingModule());

  it('renders the universe and removes only manual entries', async () => {
    const api = apiMock();
    api.getUniverse.mockResolvedValue([
      { symbol: 'MSFT', name: 'Microsoft', sector: 'Tech', source: 'sp500', excluded: false },
      { symbol: 'XYZ', name: 'Мій тикер', sector: '—', source: 'manual', excluded: false },
    ]);
    api.setExcluded.mockResolvedValue({
      symbol: 'MSFT',
      name: 'Microsoft',
      sector: 'Tech',
      source: 'sp500',
      excluded: true,
    });
    api.removeSymbol.mockResolvedValue(undefined);
    configure(api);

    const fixture = TestBed.createComponent(UniverseSection);
    fixture.detectChanges();

    await vi.waitFor(() => {
      expect((fixture.nativeElement as HTMLElement).querySelectorAll('tbody tr').length).toBe(2);
    });

    // Видалення лише для manual.
    const removeButtons = [
      ...(fixture.nativeElement as HTMLElement).querySelectorAll<HTMLButtonElement>(
        'button[data-fieldkey="remove"]',
      ),
    ];
    expect(removeButtons.length).toBe(1);
    removeButtons[0].click();
    await vi.waitFor(() => expect(api.removeSymbol).toHaveBeenCalledWith('XYZ'));

    // Перемикач «виключено».
    const excluded = (fixture.nativeElement as HTMLElement).querySelector(
      'input[name="MSFT-excluded"]',
    ) as HTMLInputElement;
    excluded.checked = true;
    excluded.dispatchEvent(new Event('change'));
    await vi.waitFor(() => expect(api.setExcluded).toHaveBeenCalledWith('MSFT', true));
  });

  it('add symbol calls addSymbol and clears the input', async () => {
    const api = apiMock();
    api.addSymbol.mockResolvedValue({
      symbol: 'NVDA',
      name: 'Nvidia',
      sector: 'Tech',
      source: 'manual',
      excluded: false,
    });
    configure(api);

    const fixture = TestBed.createComponent(UniverseSection);
    fixture.detectChanges();
    await vi.waitFor(() => {
      const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
      expect(text).toContain('Юніверс порожній');
    });

    const input = (fixture.nativeElement as HTMLElement).querySelector(
      'input[name="new-symbol"]',
    ) as HTMLInputElement;
    input.value = 'nvda';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();

    (
      (fixture.nativeElement as HTMLElement).querySelector('.add button') as HTMLButtonElement
    ).click();

    await vi.waitFor(() => expect(api.addSymbol).toHaveBeenCalledWith('NVDA'));
    await vi.waitFor(() => expect(input.value).toBe(''));
  });

  it('csv import calls importUniverse with file text', async () => {
    const api = apiMock();
    api.importUniverse.mockResolvedValue({ added: 1, skipped: 0 });
    configure(api);

    const fixture = TestBed.createComponent(UniverseSection);
    fixture.detectChanges();
    await vi.waitFor(() => expect(api.getUniverse).toHaveBeenCalled());

    const input = (fixture.nativeElement as HTMLElement).querySelector(
      'input[type="file"]',
    ) as HTMLInputElement;
    const file = new File(['SPY,ETF\n'], 'universe.csv', { type: 'text/csv' });
    Object.defineProperty(input, 'files', { value: [file] });
    input.dispatchEvent(new Event('change'));

    await vi.waitFor(() => expect(api.importUniverse).toHaveBeenCalledWith('SPY,ETF\n'));
    // Повідомлення зʼявляється після перезавантаження юніверсу.
    await vi.waitFor(() => {
      fixture.detectChanges();
      const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
      expect(text).toContain('додано 1');
    });
  });
});
