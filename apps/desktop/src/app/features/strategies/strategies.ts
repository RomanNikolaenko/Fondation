import { Component, OnInit, inject, signal } from '@angular/core';

import { errorMessage } from '../../core/api-error';
import { EngineService } from '../../core/engine.service';
import type { EngineConfig } from '../../core/generated/EngineConfig';
import type { StrategiesConfig } from '../../core/generated/StrategiesConfig';
import type { TrendPullbackParams } from '../../core/generated/TrendPullbackParams';
import type { BreakoutParams } from '../../core/generated/BreakoutParams';

type TpKey =
  | 'sma_long'
  | 'sma_mid'
  | 'rsi_period'
  | 'rsi_entry'
  | 'atr_period'
  | 'atr_stop_mult'
  | 'exit_sma'
  | 'max_hold_days';
type BoKey =
  | 'entry_lookback'
  | 'volume_mult'
  | 'volume_avg'
  | 'atr_period'
  | 'atr_stop_mult'
  | 'trail_lookback';

function inputValue(event: Event): string {
  return (event.target as HTMLInputElement).value;
}

function boolValue(event: Event): boolean {
  return (event.target as HTMLInputElement).checked;
}

function numValue(event: Event): number {
  return Number(inputValue(event));
}

/** Клієнтська перевірка стратегій перед `putConfig`. */
export function validateStrategies(s: StrategiesConfig): string[] {
  const errors: string[] = [];
  const sum = (s.trend_pullback.allocation + s.breakout.allocation) * 100;
  if (sum > 100 + 1e-9) {
    errors.push(`Сума часток стратегій ${sum.toFixed(0)}% перевищує 100%`);
  }
  const requirePositive = (label: string, value: number): void => {
    if (!(value > 0)) {
      errors.push(`${label} має бути більше 0`);
    }
  };
  requirePositive('Період sma_long', s.trend_pullback.sma_long);
  requirePositive('Період sma_mid', s.trend_pullback.sma_mid);
  requirePositive('Період rsi_period', s.trend_pullback.rsi_period);
  requirePositive('Період atr_period', s.trend_pullback.atr_period);
  requirePositive('Період exit_sma', s.trend_pullback.exit_sma);
  requirePositive('Максимальна тривалість угоди', s.trend_pullback.max_hold_days);
  requirePositive('Вікно пробою', s.breakout.entry_lookback);
  requirePositive('Середня обсягів', s.breakout.volume_avg);
  requirePositive('Період atr_period', s.breakout.atr_period);
  requirePositive('Вікно трейлінга', s.breakout.trail_lookback);
  requirePositive('Період ринкового фільтра sma', s.market_filter.sma);
  return errors;
}

/** Екран «Стратегії»: параметри стратегій і ринкового фільтра. */
@Component({
  selector: 'app-strategies',
  standalone: true,
  templateUrl: './strategies.html',
  styleUrl: './strategies.scss',
})
export class Strategies implements OnInit {
  private readonly api = inject(EngineService);

  protected readonly config = signal<EngineConfig | null>(null);
  protected readonly strategies = signal<StrategiesConfig | null>(null);

  protected readonly clientErrors = signal<string[]>([]);
  protected readonly saving = signal(false);
  protected readonly saved = signal(false);
  protected readonly error = signal<string | null>(null);

  ngOnInit(): void {
    void this.load();
  }

  private async load(): Promise<void> {
    try {
      const config = await this.api.getConfig();
      this.config.set(config);
      this.strategies.set(JSON.parse(JSON.stringify(config.strategies)) as StrategiesConfig);
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  // ---------------------------------------------------------- редагування

  protected setTpEnabled(event: Event): void {
    this.strategies.update((s) =>
      this.withGroup(s, (draft) => {
        const tp = draft.trend_pullback as TrendPullbackParams & { enabled?: boolean };
        tp.enabled = boolValue(event);
      }),
    );
  }

  protected setTpAlloc(event: Event): void {
    const value = numValue(event) / 100;
    this.strategies.update((s) =>
      this.withGroup(s, (draft) => {
        draft.trend_pullback.allocation = value;
      }),
    );
  }

  protected setTp(key: TpKey, event: Event): void {
    const value = numValue(event);
    this.strategies.update((s) =>
      this.withGroup(s, (draft) => {
        (draft.trend_pullback as Record<TpKey, number>)[key] = value;
      }),
    );
  }

  protected setBoEnabled(event: Event): void {
    this.strategies.update((s) =>
      this.withGroup(s, (draft) => {
        const bo = draft.breakout as BreakoutParams & { enabled?: boolean };
        bo.enabled = boolValue(event);
      }),
    );
  }

  protected setBoAlloc(event: Event): void {
    const value = numValue(event) / 100;
    this.strategies.update((s) =>
      this.withGroup(s, (draft) => {
        (draft.breakout as Record<'allocation', number>)['allocation'] = value;
      }),
    );
  }

  protected setBo(key: BoKey, event: Event): void {
    const value = numValue(event);
    this.strategies.update((s) =>
      this.withGroup(s, (draft) => {
        (draft.breakout as Record<BoKey, number>)[key] = value;
      }),
    );
  }

  protected setMarketSymbol(event: Event): void {
    const value = inputValue(event).trim().toUpperCase();
    this.strategies.update((s) =>
      this.withGroup(s, (draft) => {
        draft.market_filter.symbol = value;
      }),
    );
  }

  protected setMarketSma(event: Event): void {
    const value = numValue(event);
    this.strategies.update((s) =>
      this.withGroup(s, (draft) => {
        draft.market_filter.sma = value;
      }),
    );
  }

  private withGroup(
    source: StrategiesConfig | null,
    mutate: (draft: StrategiesConfig) => void,
  ): StrategiesConfig | null {
    if (!source) {
      return source;
    }
    const draft = JSON.parse(JSON.stringify(source)) as StrategiesConfig;
    mutate(draft);
    return draft;
  }

  // ---------------------------------------------------------- збереження

  protected async save(): Promise<void> {
    const strategies = this.strategies();
    const config = this.config();
    if (!strategies || !config) {
      return;
    }
    const errors = validateStrategies(strategies);
    this.clientErrors.set(errors);
    if (errors.length > 0) {
      return;
    }
    this.error.set(null);
    this.saving.set(true);
    try {
      const updated = await this.api.putConfig({ ...config, strategies });
      this.config.set(updated);
      this.strategies.set(JSON.parse(JSON.stringify(updated.strategies)) as StrategiesConfig);
      this.saved.set(true);
    } catch (error) {
      this.saved.set(false);
      this.error.set(errorMessage(error));
    } finally {
      this.saving.set(false);
    }
  }

  protected allocPct(value: number | undefined): number {
    return (value ?? 0) * 100;
  }
}
