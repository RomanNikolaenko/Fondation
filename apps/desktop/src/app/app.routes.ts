import { Routes } from '@angular/router';

import { disclaimerGuard } from './core/disclaimer.guard';
import { UK } from './i18n/uk';

const placeholder = () =>
  import('./features/placeholder').then((m) => m.Placeholder);

const overview = () =>
  import('./features/overview/overview').then((m) => m.Overview);

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
      { path: '', pathMatch: 'full', loadComponent: overview },
      {
        path: 'approvals',
        loadComponent: () => import('./features/approvals/approvals').then((m) => m.Approvals),
      },
      {
        path: 'positions',
        loadComponent: () => import('./features/positions/positions').then((m) => m.Positions),
      },
      {
        path: 'signals',
        loadComponent: placeholder,
        data: { title: UK.nav.signals },
      },
      { path: 'chart', loadComponent: () => import('./features/chart/chart').then((m) => m.Chart) },
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
