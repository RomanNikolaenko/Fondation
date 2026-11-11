import { Component, OnInit, inject, signal } from '@angular/core';

import { errorMessage } from '../../core/api-error';
import { EngineService } from '../../core/engine.service';
import type { EngineConfig } from '../../core/generated/EngineConfig';
import type { AiReviewConfig } from '../../core/generated/AiReviewConfig';
import type { ExecutionMode } from '../../core/generated/ExecutionMode';
import type { NewsFilterConfig } from '../../core/generated/NewsFilterConfig';
import type { RiskConfig } from '../../core/generated/RiskConfig';
import type { ScheduleConfig } from '../../core/generated/ScheduleConfig';
import { etWallTimeToUtc, todayEt } from '../../core/tz';
import { KeysSection } from './keys-section';
import { UniverseSection } from './universe-section';

const RISK_PERCENT_KEYS = [
  'risk_per_trade',
  'max_position_pct',
  'max_sector_pct',
  'max_gross_exposure',
  'daily_loss_limit',
  'weekly_loss_limit',
  'price_sanity_pct',
  'max_gap',
] as const;

export type RiskPercentKey = (typeof RISK_PERCENT_KEYS)[number];
export type RiskPercentSource = Pick<RiskConfig, RiskPercentKey>;

/** Поля ризику, які у конфігу — частки, а в інтерфейсі — відсотки. */
export const RISK_PERCENT_FIELDS: ReadonlyArray<{
  key: RiskPercentKey;
  label: string;
}> = [
  { key: 'risk_per_trade', label: 'Ризик на угоду, %' },
  { key: 'max_position_pct', label: 'Максимальна позиція, %' },
  { key: 'max_sector_pct', label: 'Максимальний сектор, %' },
  { key: 'max_gross_exposure', label: 'Максимальна сукупна експозиція, %' },
  { key: 'daily_loss_limit', label: 'Денний ліміт збитку, %' },
  { key: 'weekly_loss_limit', label: 'Тижневий ліміт збитку, %' },
  { key: 'price_sanity_pct', label: 'Допустиме відхилення ціни, %' },
  { key: 'max_gap', label: 'Максимальний геп, %' },
];

/** Частки ризику → відсотки для показу (0.01 → 1). */
export function riskToPercent(risk: RiskPercentSource): Record<string, number> {
  const percents: Record<string, number> = {};
  for (const field of RISK_PERCENT_FIELDS) {
    percents[field.key] = risk[field.key] * 100;
  }
  return percents;
}

/** Відсотки з інтерфейсу → частки для конфігу (1 → 0.01). */
export function riskToFraction(percents: Record<string, number>): Record<string, number> {
  const fractions: Record<string, number> = {};
  for (const field of RISK_PERCENT_FIELDS) {
    const fraction = (percents[field.key] ?? 0) / 100;
    fractions[field.key] = Math.round(fraction * 1e9) / 1e9;
  }
  return fractions;
}

/** Підказка київського часу для настінного часу ET («08:55» → «15:55»). */
export function kyivHint(etTime: string): string {
  try {
    const at = etWallTimeToUtc(todayEt(), etTime);
    return new Intl.DateTimeFormat('en-GB', {
      timeZone: 'Europe/Kyiv',
      hourCycle: 'h23',
      hour: '2-digit',
      minute: '2-digit',
    }).format(at);
  } catch {
    return '—';
  }
}

const AUTO_WARNING =
  'Увімкнути автоматичне виконання? У режимі «авто» угоди надсилаються ' +
  'брокеру одразу після ризик-перевірок, без підтвердження з вашого боку.';

function inputValue(event: Event): string {
  return (event.target as HTMLInputElement).value;
}

function numValue(event: Event): number {
  return Number(inputValue(event));
}

function boolValue(event: Event): boolean {
  return (event.target as HTMLInputElement).checked;
}

/** Екран «Налаштування»: ключі, режим, ліміти, розклад, фільтри, AI, юніверс. */
@Component({
  selector: 'app-settings',
  standalone: true,
  imports: [KeysSection, UniverseSection],
  templateUrl: './settings.html',
  styleUrl: './settings.scss',
})
export class Settings implements OnInit {
  private readonly api = inject(EngineService);

  protected readonly riskFields = RISK_PERCENT_FIELDS;

  protected readonly config = signal<EngineConfig | null>(null);
  protected readonly riskPct = signal<Record<string, number>>({});
  protected readonly autostart = signal(false);

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
      this.riskPct.set(riskToPercent(config.risk));
    } catch (error) {
      this.error.set(errorMessage(error));
    }
    try {
      this.autostart.set(await this.api.getAutostart());
    } catch {
      // Автозапуск може бути недоступний — некритично.
    }
  }

  private patch(mutate: (draft: EngineConfig) => void): void {
    this.config.update((current) => {
      if (!current) {
        return current;
      }
      const draft = JSON.parse(JSON.stringify(current)) as EngineConfig;
      mutate(draft);
      return draft;
    });
  }

  // ---------------------------------------------------------- ключі/режим

  protected onModeChange(event: Event): void {
    const select = event.target as HTMLSelectElement;
    const wanted: ExecutionMode = select.value === 'auto' ? 'auto' : 'approval';
    if (wanted === 'auto' && this.config()?.execution_mode !== 'auto' && !confirm(AUTO_WARNING)) {
      select.value = this.config()?.execution_mode ?? 'approval';
      return;
    }
    this.patch((draft) => {
      draft.execution_mode = wanted;
    });
  }

  protected currentMode(): string {
    return this.config()?.execution_mode ?? 'approval';
  }

  // ---------------------------------------------------------- ризик

  protected setRisk(key: string, event: Event): void {
    const value = numValue(event);
    this.riskPct.update((state) => ({ ...state, [key]: value }));
    this.patch((draft) => {
      (draft.risk as unknown as Record<string, number>)[key] = value / 100;
    });
  }

  protected setRiskInt(key: string, event: Event): void {
    const value = Math.max(0, Math.round(numValue(event) || 0));
    this.patch((draft) => {
      (draft.risk as unknown as Record<string, number>)[key] = value;
    });
  }

  protected riskInteger(key: 'max_open_positions' | 'max_orders_per_day'): number {
    return this.config()?.risk[key] ?? 0;
  }

  protected riskPercent(key: string): number {
    const value = this.riskPct()[key];
    return Number.isFinite(value) ? value : 0;
  }

  // ---------------------------------------------------------- розклад

  protected setSchedule(key: keyof ScheduleConfig, event: Event): void {
    const value: string | number =
      key === 'reconcile_every_minutes'
        ? Math.max(1, Math.round(numValue(event) || 0))
        : inputValue(event);
    this.patch((draft) => {
      (draft.schedule as Record<string, string | number>)[key] = value;
    });
  }

  protected hint(key: keyof ScheduleConfig): string {
    const time = this.config()?.schedule[key];
    return typeof time === 'string' && time !== '' ? kyivHint(time) : '—';
  }

  protected scheduleValue(key: keyof ScheduleConfig): string | number {
    const value = this.config()?.schedule[key];
    return value ?? '';
  }

  protected scheduleTime(key: keyof ScheduleConfig): string {
    const value = this.config()?.schedule[key];
    return typeof value === 'string' ? value : '';
  }

  // ---------------------------------------------------------- фільтри

  protected setNewsBoolean(key: 'enabled', event: Event): void {
    this.patch((draft) => {
      draft.news_filter[key] = boolValue(event);
    });
  }

  protected setNews(key: 'model' | 'caution_in_auto' | 'on_unavailable', event: Event): void {
    const value = inputValue(event);
    this.patch((draft) => {
      (draft.news_filter as unknown as Record<string, string>)[key] = value;
    });
  }

  protected setNewsNumber(key: 'daily_budget_usd' | 'monthly_budget_usd', event: Event): void {
    const value = numValue(event);
    this.patch((draft) => {
      (draft.news_filter as unknown as Record<string, number>)[key] = value;
    });
  }

  protected news(key: keyof NewsFilterConfig): unknown {
    return this.config()?.news_filter[key];
  }

  protected stagedNews(): NewsFilterConfig | null {
    return this.config()?.news_filter ?? null;
  }

  protected newsValue(key: string): string | number | boolean {
    const filter = this.stagedNews();
    return (filter as unknown as Record<string, string | number | boolean>)[key];
  }

  protected setEarningsBoolean(key: 'enabled', event: Event): void {
    this.patch((draft) => {
      draft.earnings_filter[key] = boolValue(event);
    });
  }

  protected setEarningsNumber(key: 'days_ahead', event: Event): void {
    const value = Math.max(0, Math.round(numValue(event) || 0));
    this.patch((draft) => {
      (draft.earnings_filter as unknown as Record<string, number>)[key] = value;
    });
  }

  protected earningsValue(key: string): string | number | boolean {
    const filter = this.config()?.earnings_filter;
    return (filter as unknown as Record<string, string | number | boolean>)?.[key] ?? '';
  }

  protected setEarningsPolicy(event: Event): void {
    const value = inputValue(event);
    this.patch((draft) => {
      draft.earnings_filter.on_unavailable = value === 'allow' ? 'allow' : 'block';
    });
  }

  protected newsPolicies(): Array<{ value: string; label: string }> {
    return [
      { value: 'block', label: 'блок угод' },
      { value: 'allow', label: 'дозволити' },
    ];
  }

  // ---------------------------------------------------------- AI-аналітик

  protected setAiBoolean(key: 'enabled', event: Event): void {
    this.patch((draft) => {
      draft.ai[key] = boolValue(event);
    });
  }

  protected setAi(key: 'model' | 'effort', event: Event): void {
    const value = inputValue(event);
    this.patch((draft) => {
      (draft.ai as unknown as Record<string, string>)[key] = value;
    });
  }

  protected setAiNumber(
    key: 'context_bars' | 'max_proposals' | 'lookback_days',
    event: Event,
  ): void {
    const value = Math.max(0, Math.round(numValue(event) || 0));
    this.patch((draft) => {
      (draft.ai as unknown as Record<string, number>)[key] = value;
    });
  }

  /** Частка стратегії `ai` в інтерфейсі — відсотки (0.25 → 25). */
  protected aiAllocationPercent(): number {
    return (this.config()?.ai.allocation ?? 0) * 100;
  }

  protected setAiAllocation(event: Event): void {
    const fraction = numValue(event) / 100;
    this.patch((draft) => {
      draft.ai.allocation = Math.round(fraction * 1e9) / 1e9;
    });
  }

  protected aiValue(key: keyof AiReviewConfig): string | number | boolean {
    const ai = this.config()?.ai;
    return (ai as unknown as Record<string, string | number | boolean>)?.[key] ?? '';
  }

  protected aiEfforts(): Array<{ value: string; label: string }> {
    return [
      { value: 'low', label: 'низький' },
      { value: 'medium', label: 'середній' },
      { value: 'high', label: 'високий' },
    ];
  }

  // ---------------------------------------------------------- система

  protected async setAutostart(event: Event): Promise<void> {
    const value = boolValue(event);
    try {
      await this.api.setAutostart(value);
      this.autostart.set(value);
    } catch (error) {
      this.error.set(errorMessage(error));
    }
  }

  protected setNotifications(event: Event): void {
    const value = boolValue(event);
    this.patch((draft) => {
      draft.ui.notifications_enabled = value;
    });
  }

  protected autostartError(): string | null {
    return this.error();
  }

  // ---------------------------------------------------------- збереження

  protected async save(): Promise<void> {
    const config = this.config();
    if (!config) {
      return;
    }
    this.clientErrors.set([]);
    this.error.set(null);
    this.saving.set(true);
    try {
      const updated = await this.api.putConfig(config);
      this.config.set(updated);
      this.saved.set(true);
    } catch (error) {
      this.saved.set(false);
      this.error.set(errorMessage(error));
    } finally {
      this.saving.set(false);
    }
  }
}
