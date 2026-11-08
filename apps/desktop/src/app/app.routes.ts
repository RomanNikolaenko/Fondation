import { Routes } from '@angular/router';

import { disclaimerGuard } from './core/disclaimer.guard';
import { UK } from './i18n/uk';

const placeholder = () =>
  import('./features/placeholder').then((m) => m.Placeholder);

export const routes: Routes = [
  {
    path: 'welcome',
    loadComponent: () => import('./features/welcome/welcome').then((m) => m.Welcome),
  },
  {
    path: '',
    loadComponent: () => import('./shell/shell').then((m) => m.Shell),
    canActivate: [disclaimerGuard],
    children: [
      { path: '', pathMatch: 'full', loadComponent: placeholder, data: { title: UK.nav.overview } },
      {
        path: 'approvals',
        loadComponent: placeholder,
        data: { title: UK.nav.approvals },
      },
      {
        path: 'positions',
        loadComponent: placeholder,
        data: { title: UK.nav.positions },
      },
      {
        path: 'signals',
        loadComponent: placeholder,
        data: { title: UK.nav.signals },
      },
      { path: 'chart', loadComponent: placeholder, data: { title: UK.nav.chart } },
      {
        path: 'journal',
        loadComponent: placeholder,
        data: { title: UK.nav.journal },
      },
      {
        path: 'backtest',
        loadComponent: placeholder,
        data: { title: UK.nav.backtest },
      },
      {
        path: 'strategies',
        loadComponent: placeholder,
        data: { title: UK.nav.strategies },
      },
      {
        path: 'settings',
        loadComponent: placeholder,
        data: { title: UK.nav.settings },
      },
    ],
  },
  { path: '**', redirectTo: '' },
];
