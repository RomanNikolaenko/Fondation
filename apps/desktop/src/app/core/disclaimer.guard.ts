import { inject } from '@angular/core';
import type { CanActivateFn } from '@angular/router';
import { Router } from '@angular/router';

import { EngineService } from './engine.service';

/** Без прийнятого попередження всі екрани ведуть на `/welcome`. */
export const disclaimerGuard: CanActivateFn = async () => {
  const api = inject(EngineService);
  const router = inject(Router);
  try {
    const config = await api.getConfig();
    if (config.ui.disclaimer_accepted_at) {
      return true;
    }
  } catch {
    // Рушій недоступний — показуємо перший запуск (там буде видно помилку).
  }
  return router.createUrlTree(['/welcome']);
};
