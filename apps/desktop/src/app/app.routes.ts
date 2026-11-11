import { Routes } from '@angular/router';

import { disclaimerGuard } from './core/disclaimer.guard';

const overview = () => import('./features/overview/overview').then((m) => m.Overview);

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
        loadComponent: () => import('./features/signals/signals').then((m) => m.Signals),
      },
      { path: 'ai', loadComponent: () => import('./features/ai/ai').then((m) => m.Ai) },
      { path: 'chart', loadComponent: () => import('./features/chart/chart').then((m) => m.Chart) },
      {
        path: 'journal',
        loadComponent: () => import('./features/journal/journal').then((m) => m.Journal),
      },
      {
        path: 'backtest',
        loadComponent: () => import('./features/backtest/backtest').then((m) => m.Backtest),
      },
      {
        path: 'strategies',
        loadComponent: () => import('./features/strategies/strategies').then((m) => m.Strategies),
      },
      {
        path: 'settings',
        loadComponent: () => import('./features/settings/settings').then((m) => m.Settings),
      },
    ],
  },
  { path: '**', redirectTo: '' },
];
