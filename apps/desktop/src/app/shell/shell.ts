import { Component, OnInit, computed, inject, signal } from '@angular/core';
import { RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';

import { EngineStore } from '../core/engine-store';
import { UK } from '../i18n/uk';
import { ModeBanner } from './mode-banner';

interface NavItem {
  path: string;
  label: string;
}

@Component({
  selector: 'app-shell',
  standalone: true,
  imports: [RouterOutlet, RouterLink, RouterLinkActive, ModeBanner],
  templateUrl: './shell.html',
  styleUrl: './shell.scss',
})
export class Shell implements OnInit {
  protected readonly store = inject(EngineStore);
  protected readonly appName = UK.appName;

  protected readonly nav: NavItem[] = [
    { path: '', label: UK.nav.overview },
    { path: 'approvals', label: UK.nav.approvals },
    { path: 'positions', label: UK.nav.positions },
    { path: 'signals', label: UK.nav.signals },
    { path: 'ai', label: UK.nav.ai },
    { path: 'chart', label: UK.nav.chart },
    { path: 'journal', label: UK.nav.journal },
    { path: 'backtest', label: UK.nav.backtest },
    { path: 'strategies', label: UK.nav.strategies },
    { path: 'settings', label: UK.nav.settings },
  ];

  protected readonly engineLabel = computed(() => {
    const state = this.store.modeState();
    switch (state) {
      case 'running':
        return `Рушій: ${UK.engine.running}`;
      case 'paused':
        return `Рушій: ${UK.engine.paused}`;
      case 'killed':
        return `Рушій: ${UK.engine.killed}`;
      default:
        return `Рушій: ${UK.engine.unknown}`;
    }
  });

  protected readonly stateClass = computed(() => `chip--${this.store.modeState()}`);

  private readonly initialTheme: 'light' | 'dark' =
    typeof matchMedia === 'function' && matchMedia('(prefers-color-scheme: dark)').matches
      ? 'dark'
      : 'light';
  protected readonly theme = signal<'light' | 'dark'>(this.initialTheme);

  constructor() {
    this.applyTheme(this.theme());
  }

  ngOnInit(): void {
    this.store.start();
  }

  protected toggleTheme(): void {
    const next = this.theme() === 'dark' ? 'light' : 'dark';
    this.theme.set(next);
    this.applyTheme(next);
  }

  private applyTheme(theme: 'light' | 'dark'): void {
    document.documentElement.dataset['theme'] = theme;
  }
}
