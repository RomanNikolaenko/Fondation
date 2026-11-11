import { Injectable } from '@angular/core';
import { invoke } from '@tauri-apps/api/core';

import type { AccountDto } from './generated/AccountDto';
import type { AiProposalDto } from './generated/AiProposalDto';
import type { AiRunQueuedDto } from './generated/AiRunQueuedDto';
import type { BacktestDetailDto } from './generated/BacktestDetailDto';
import type { BacktestRequest } from './generated/BacktestRequest';
import type { BacktestRunDto } from './generated/BacktestRunDto';
import type { Bar } from './generated/Bar';
import type { BrokerOrder } from './generated/BrokerOrder';
import type { EngineConfig } from './generated/EngineConfig';
import type { HistoryJobDto } from './generated/HistoryJobDto';
import type { ImportResultDto } from './generated/ImportResultDto';
import type { Intent } from './generated/Intent';
import type { IntentStatus } from './generated/IntentStatus';
import type { JournalEventDto } from './generated/JournalEventDto';
import type { KillResultDto } from './generated/KillResultDto';
import type { PositionDto } from './generated/PositionDto';
import type { ScanResultDto } from './generated/ScanResultDto';
import type { SecretStatusDto } from './generated/SecretStatusDto';
import type { SignalDto } from './generated/SignalDto';
import type { StatusDto } from './generated/StatusDto';
import type { UniverseEntryDto } from './generated/UniverseEntryDto';
import type { VerifyResultDto } from './generated/VerifyResultDto';

/** Параметри журналу; `null`/`undefined` не передаються. */
export interface JournalQuery {
  from?: string | null;
  to?: string | null;
  symbol?: string | null;
  kinds?: string | null;
  limit?: number | null;
}

/**
 * Один метод на команду Tauri (Task 23). Імена аргументів — camelCase,
 * як їх очікує `#[tauri::command]`.
 */
@Injectable({ providedIn: 'root' })
export class EngineService {
  private call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    return invoke<T>(command, args);
  }

  getStatus(): Promise<StatusDto> {
    return this.call('get_status');
  }

  getAccount(): Promise<AccountDto> {
    return this.call('get_account');
  }

  getPositions(): Promise<PositionDto[]> {
    return this.call('get_positions');
  }

  getOrders(): Promise<BrokerOrder[]> {
    return this.call('get_orders');
  }

  getSignals(date: string): Promise<SignalDto[]> {
    return this.call('get_signals', { date });
  }

  getScan(date: string): Promise<ScanResultDto[]> {
    return this.call('get_scan', { date });
  }

  getAiProposals(days?: number): Promise<AiProposalDto[]> {
    return this.call('get_ai_proposals', days === undefined ? {} : { days });
  }

  runAiAnalysis(): Promise<AiRunQueuedDto> {
    return this.call('run_ai_analysis');
  }

  getIntents(statuses: IntentStatus[] = []): Promise<Intent[]> {
    return this.call('get_intents', { statuses });
  }

  approveIntent(id: string): Promise<Intent> {
    return this.call('approve_intent', { id });
  }

  rejectIntent(id: string): Promise<Intent> {
    return this.call('reject_intent', { id });
  }

  closePosition(symbol: string): Promise<Intent> {
    return this.call('close_position', { symbol });
  }

  pauseEngine(): Promise<StatusDto> {
    return this.call('pause_engine');
  }

  resumeEngine(): Promise<StatusDto> {
    return this.call('resume_engine');
  }

  killEngine(closePositions: boolean): Promise<KillResultDto> {
    return this.call('kill_engine', { closePositions });
  }

  getConfig(): Promise<EngineConfig> {
    return this.call('get_config');
  }

  putConfig(config: EngineConfig): Promise<EngineConfig> {
    return this.call('put_config', { config });
  }

  getBars(symbol: string, from?: string, to?: string): Promise<Bar[]> {
    return this.call('get_bars', { symbol, from, to });
  }

  getJournal(query: JournalQuery = {}): Promise<JournalEventDto[]> {
    return this.call('get_journal', { query });
  }

  startBacktest(request: BacktestRequest): Promise<BacktestRunDto> {
    return this.call('start_backtest', { request });
  }

  listBacktests(): Promise<BacktestRunDto[]> {
    return this.call('list_backtests');
  }

  getBacktest(id: string): Promise<BacktestDetailDto> {
    return this.call('get_backtest', { id });
  }

  startHistoryDownload(from: string): Promise<HistoryJobDto> {
    return this.call('start_history_download', { from });
  }

  getHistoryJob(): Promise<HistoryJobDto> {
    return this.call('get_history_job');
  }

  getUniverse(): Promise<UniverseEntryDto[]> {
    return this.call('get_universe');
  }

  addSymbol(symbol: string): Promise<UniverseEntryDto> {
    return this.call('add_symbol', { symbol });
  }

  setExcluded(symbol: string, excluded: boolean): Promise<UniverseEntryDto> {
    return this.call('set_excluded', { symbol, excluded });
  }

  removeSymbol(symbol: string): Promise<void> {
    return this.call('remove_symbol', { symbol });
  }

  importUniverse(csv: string): Promise<ImportResultDto> {
    return this.call('import_universe', { csv });
  }

  getSecrets(): Promise<SecretStatusDto[]> {
    return this.call('get_secrets');
  }

  setSecret(key: string, value: string): Promise<void> {
    return this.call('set_secret', { key, value });
  }

  verifySecrets(): Promise<VerifyResultDto> {
    return this.call('verify_secrets');
  }

  getAutostart(): Promise<boolean> {
    return this.call('get_autostart');
  }

  setAutostart(enabled: boolean): Promise<void> {
    return this.call('set_autostart', { enabled });
  }
}
