import { TestBed } from '@angular/core/testing';
import { UrlTree, provideRouter } from '@angular/router';

import { disclaimerGuard } from './disclaimer.guard';
import { EngineService } from './engine.service';
import type { EngineConfig } from './generated/EngineConfig';

function configWith(acceptedAt: string | null): EngineConfig {
  return {
    ui: { disclaimer_accepted_at: acceptedAt, notifications_enabled: true },
  } as EngineConfig;
}

describe('disclaimerGuard', () => {
  afterEach(() => TestBed.resetTestingModule());

  it('redirects to /welcome when disclaimer not accepted', async () => {
    const api = { getConfig: vi.fn().mockResolvedValue(configWith(null)) };
    TestBed.configureTestingModule({
      providers: [provideRouter([]), { provide: EngineService, useValue: api }],
    });

    const result = await TestBed.runInInjectionContext(() =>
      disclaimerGuard({} as never, {} as never),
    );

    expect(result).toBeInstanceOf(UrlTree);
    expect(String(result)).toBe('/welcome');
  });

  it('allows when accepted', async () => {
    const api = {
      getConfig: vi
        .fn()
        .mockResolvedValue(configWith('2026-11-05T13:00:00.000Z')),
    };
    TestBed.configureTestingModule({
      providers: [provideRouter([]), { provide: EngineService, useValue: api }],
    });

    const result = await TestBed.runInInjectionContext(() =>
      disclaimerGuard({} as never, {} as never),
    );

    expect(result).toBe(true);
  });
});
