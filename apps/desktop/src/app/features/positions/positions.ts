import { Component, DestroyRef, OnInit, computed, inject, signal } from '@angular/core';
import { RouterLink } from '@angular/router';

import { errorMessage } from '../../core/api-error';
import { EngineService } from '../../core/engine.service';
import { EventsService } from '../../core/events.service';
import type { BrokerOrder } from '../../core/generated/BrokerOrder';
import type { PositionDto } from '../../core/generated/PositionDto';
import { MoneyPipe } from '../../shared/money.pipe';

const OPEN: string[] = [
  'new',
  'accepted',
  'pending_new',
  'held',
  'partially_filled',
];

function isOpen(status: BrokerOrder['status']): boolean {
  return typeof status === 'string' && OPEN.includes(status);
}

/** Екран «Позиції»: таблиця позицій і відкриті ордери брокера. */
@Component({
  selector: 'app-positions',
  standalone: true,
  imports: [MoneyPipe, RouterLink],
  templateUrl: './positions.html',
  styleUrl: './positions.scss',
})
export class Positions implements OnInit {
  private readonly api = inject(EngineService);
  private readonly events = inject(EventsService);
  private readonly destroyRef = inject(DestroyRef);

  protected readonly positions = signal<PositionDto[]>([]);
  protected readonly orders = signal<BrokerOrder[]>([]);
  protected readonly closing = signal<string | null>(null);
  protected readonly error = signal<string | null>(null);

  /** Відкриті ордери (не filled/canceled/expired). */
  protected readonly openOrders = computed(() =>
    this.orders().filter((order) => isOpen(order.status)),
  );

  protected readonly loading = signal(true);

  ngOnInit(): void {
    void this.reload();
    this.destroyRef.onDestroy(
      this.events.on('order_filled', () => void this.reload()),
    );
    this.destroyRef.onDestroy(
      this.events.on('stop_hit', () => void this.reload()),
    );
  }

  protected positionValue(position: PositionDto): number {
    return position.qty * Number(position.current_price);
  }

  /**
   * Закриття позиції за ринком; вимагає підтвердження користувача.
   * Повертає `false`, якщо підтвердження відхилено або викликається поза браузером.
   */
  protected async close(position: PositionDto): Promise<void> {
    if (this.closing() !== null) {
      return;
    }
    this.closing.set(position.symbol);
    this.error.set(null);
    try {
      if (!confirm(`Закрити позицію ${position.symbol} за ринком?`)) {
        return;
      }
      await this.api.closePosition(position.symbol);
      await this.reload();
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.closing.set(null);
    }
  }

  private async reload(): Promise<void> {
    try {
      const [positions, orders] = await Promise.all([
        this.api.getPositions(),
        this.api.getOrders(),
      ]);
      this.positions.set(positions);
      this.orders.set(orders);
      this.error.set(null);
    } catch (error) {
      this.error.set(errorMessage(error));
    } finally {
      this.loading.set(false);
    }
  }
}
