import { TestBed } from '@angular/core/testing';
import { Router } from '@angular/router';

import { EngineService } from '../../core/engine.service';
import type { EngineConfig } from '../../core/generated/EngineConfig';
import { Welcome } from './welcome';

describe('Welcome', () => {
  afterEach(() => TestBed.resetTestingModule());

  it('accept writes timestamp via putConfig', async () => {
    const config = {
      ui: { disclaimer_accepted_at: null, notifications_enabled: true },
    } as EngineConfig;
    const api = {
      getConfig: vi.fn().mockResolvedValue(config),
      putConfig: vi.fn().mockResolvedValue(config),
    };
    const router = { navigateByUrl: vi.fn().mockResolvedValue(true) };

    TestBed.configureTestingModule({
      imports: [Welcome],
      providers: [
        { provide: EngineService, useValue: api },
        { provide: Router, useValue: router },
      ],
    });

    const fixture = TestBed.createComponent(Welcome);
    fixture.detectChanges();

    const checkbox = fixture.nativeElement.querySelector(
      'input[type="checkbox"]',
    ) as HTMLInputElement;
    checkbox.click();
    fixture.detectChanges();

    const button = fixture.nativeElement.querySelector('button') as HTMLButtonElement;
    expect(button.disabled).toBe(false);
    button.click();

    await vi.waitFor(() => expect(api.putConfig).toHaveBeenCalled());
    const sent = api.putConfig.mock.calls[0]?.[0] as EngineConfig;
    expect(sent.ui.disclaimer_accepted_at).toMatch(/^\d{4}-\d{2}-\d{2}T/);
    expect(router.navigateByUrl).toHaveBeenCalledWith('/');
  });
});
