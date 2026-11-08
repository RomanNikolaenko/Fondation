import { Component, inject, signal } from '@angular/core';
import { Router } from '@angular/router';

import { errorMessage } from '../../core/api-error';
import { EngineService } from '../../core/engine.service';
import { UK } from '../../i18n/uk';

@Component({
  selector: 'app-welcome',
  standalone: true,
  template: `
    <section class="welcome">
      <article class="card">
        <h1>{{ text.appName }}</h1>
        <h2>{{ text.title }}</h2>
        <p class="disclaimer">{{ text.text }}</p>

        <label class="accept">
          <input type="checkbox" [checked]="accepted()" (change)="toggle($event)" />
          <span>{{ text.acceptLabel }}</span>
        </label>

        @if (error()) {
          <p class="error" role="alert">{{ error() }}</p>
        }

        <button type="button" [disabled]="!accepted() || saving()" (click)="accept()">
          {{ saving() ? text.saving : text.button }}
        </button>
      </article>
    </section>
  `,
  styles: `
    .welcome {
      display: grid;
      place-items: center;
      min-height: 100vh;
      padding: 2rem;
      background: var(--bg);
    }
    .card {
      width: min(640px, 100%);
      padding: 2rem;
      border: 1px solid var(--border);
      border-radius: 0.9rem;
      background: var(--surface);
      box-shadow: 0 10px 30px rgb(0 0 0 / 8%);
    }
    h1 {
      margin: 0 0 1rem;
      color: var(--accent);
      font-size: 1.6rem;
    }
    h2 {
      margin: 0 0 0.75rem;
      font-size: 1.2rem;
    }
    .disclaimer {
      margin: 0 0 1.25rem;
      color: var(--text-muted);
      line-height: 1.55;
    }
    .accept {
      display: flex;
      align-items: flex-start;
      gap: 0.6rem;
      margin-bottom: 1.25rem;
      cursor: pointer;
    }
    button {
      width: 100%;
      padding: 0.7rem 1rem;
      border: 0;
      border-radius: 0.55rem;
      background: var(--accent);
      color: #fff;
      font-size: 1rem;
      cursor: pointer;
    }
    button:disabled {
      cursor: not-allowed;
      opacity: 0.5;
    }
    .error {
      margin: 0 0 1rem;
      color: #b91c1c;
    }
  `,
})
export class Welcome {
  private readonly api = inject(EngineService);
  private readonly router = inject(Router);

  protected readonly text = {
    ...UK.disclaimer,
    appName: UK.appName,
  };
  protected readonly accepted = signal(false);
  protected readonly saving = signal(false);
  protected readonly error = signal<string | null>(null);

  protected toggle(event: Event): void {
    this.accepted.set((event.target as HTMLInputElement).checked);
  }

  protected async accept(): Promise<void> {
    if (!this.accepted() || this.saving()) {
      return;
    }
    this.saving.set(true);
    this.error.set(null);
    try {
      const config = await this.api.getConfig();
      config.ui = {
        ...config.ui,
        disclaimer_accepted_at: new Date().toISOString(),
      };
      await this.api.putConfig(config);
      await this.router.navigateByUrl('/');
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.saving.set(false);
    }
  }
}
