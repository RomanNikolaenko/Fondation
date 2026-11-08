import { Component } from '@angular/core';

import { UK } from '../i18n/uk';

/** Постійна смуга, що нагадує: торгівля лише віртуальна. */
@Component({
  selector: 'app-mode-banner',
  standalone: true,
  template: `<div class="mode-banner" role="status">{{ text }}</div>`,
  styles: `
    .mode-banner {
      background: #b45309;
      color: #fff7ed;
      padding: 0.4rem 1.25rem;
      font-size: 0.8rem;
      font-weight: 600;
      letter-spacing: 0.04em;
      text-transform: uppercase;
    }
  `,
})
export class ModeBanner {
  protected readonly text = UK.paperBanner;
}
