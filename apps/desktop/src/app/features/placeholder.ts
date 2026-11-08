import { Component, inject } from '@angular/core';
import { ActivatedRoute } from '@angular/router';

import { UK } from '../i18n/uk';

/** Тимчасовий екран для розділів, які реалізують Tasks 25–30. */
@Component({
  selector: 'app-placeholder',
  standalone: true,
  template: `
    <section>
      <h1>{{ title }}</h1>
      <p class="muted">{{ note }}</p>
    </section>
  `,
  styles: `
    h1 {
      margin-top: 0;
    }
    .muted {
      color: var(--text-muted);
    }
  `,
})
export class Placeholder {
  private readonly route = inject(ActivatedRoute);
  protected readonly title = String(this.route.snapshot.data['title'] ?? '');
  protected readonly note = UK.common.inDevelopment;
}
