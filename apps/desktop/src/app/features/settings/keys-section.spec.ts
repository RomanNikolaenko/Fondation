import { TestBed } from '@angular/core/testing';

import { EngineService } from '../../core/engine.service';
import { KeysSection } from './keys-section';

type MockFn = ReturnType<typeof vi.fn>;

interface ApiMock {
  getSecrets: MockFn;
  setSecret: MockFn;
  verifySecrets: MockFn;
}

function apiMock(): ApiMock {
  return {
    getSecrets: vi.fn().mockResolvedValue([]),
    setSecret: vi.fn().mockResolvedValue(undefined),
    verifySecrets: vi.fn().mockResolvedValue({
      alpaca: { ok: false, message: 'невірний ключ' },
      finnhub: { ok: true, message: null },
      anthropic: { ok: true, message: null },
    }),
  };
}

function configure(api: ApiMock): void {
  TestBed.configureTestingModule({
    imports: [KeysSection],
    providers: [{ provide: EngineService, useValue: api }],
  });
}

describe('KeysSection', () => {
  afterEach(() => {
    TestBed.resetTestingModule();
    vi.unstubAllGlobals();
  });

  it('stored secrets are never rendered, only presence badges', async () => {
    const api = apiMock();
    api.getSecrets.mockResolvedValue([
      { key: 'alpaca_key_id', present: true },
      { key: 'alpaca_secret', present: false },
      { key: 'anthropic_api_key', present: true },
      { key: 'finnhub_api_key', present: false },
    ]);
    configure(api);

    const fixture = TestBed.createComponent(KeysSection);
    fixture.detectChanges();

    await vi.waitFor(() => {
      const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
      expect(text).toContain('збережено');
      expect(text).toContain('немає');
    });

    for (const input of [
      ...(fixture.nativeElement as HTMLElement).querySelectorAll<HTMLInputElement>(
        'input[type="password"]',
      ),
    ]) {
      expect(input.value).toBe('');
    }
  });

  it('save stores the entered value and clears the field', async () => {
    const api = apiMock();
    configure(api);

    const fixture = TestBed.createComponent(KeysSection);
    fixture.detectChanges();
    await vi.waitFor(() => expect(api.getSecrets).toHaveBeenCalled());

    const input = (fixture.nativeElement as HTMLElement).querySelector(
      'input[name="alpaca_key_id"]',
    ) as HTMLInputElement;
    input.value = 'PKTEST123';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();

    (
      (fixture.nativeElement as HTMLElement).querySelector(
        'button[data-fieldkey="alpaca_key_id"]',
      ) as HTMLButtonElement
    ).click();

    await vi.waitFor(() =>
      expect(api.setSecret).toHaveBeenCalledWith('alpaca_key_id', 'PKTEST123'),
    );
    await vi.waitFor(() => expect(input.value).toBe(''));

    const badge = (fixture.nativeElement as HTMLElement)
      .querySelector('input[name="alpaca_key_id"]')
      ?.closest('.row')
      ?.querySelector('.badge');
    await vi.waitFor(() => expect(badge?.textContent).toContain('збережено'));
  });

  it('verify shows per-service results', async () => {
    const api = apiMock();
    configure(api);

    const fixture = TestBed.createComponent(KeysSection);
    fixture.detectChanges();
    await vi.waitFor(() => expect(api.getSecrets).toHaveBeenCalled());

    (
      (fixture.nativeElement as HTMLElement).querySelector(
        'button.secondary:not([data-fieldkey])',
      ) as HTMLButtonElement
    ).click();

    await vi.waitFor(() => expect(api.verifySecrets).toHaveBeenCalledTimes(1));
    fixture.detectChanges();
    const html = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(html).toContain('Alpaca: невірний ключ');
    expect(html).toContain('Finnhub: ok');
  });
});
