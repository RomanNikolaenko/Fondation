import type { Limitation } from '../../core/generated/Limitation';

/** Дрібні частки у відсотки: 0.1234 → «12.34%». */
export function pct(x: number, digits = 2): string {
  return `${(x * 100).toFixed(digits)}%`;
}

/** Тексти обмежень оцінки для блоку «Обмеження оцінки». */
export const LIMITATION_TEXT: Record<Limitation, string> = {
  survivorship_bias:
    'Список акцій — поточний склад S&P 500: компанії, що випали з індексу, ' +
    'не враховані, тому результат може бути завищеним.',
  news_filter_not_simulated:
    'Фільтр новин (Claude) у бектесті вимкнений: модель знає, що сталося ' +
    'після історичних дат.',
  earnings_filter_not_applied:
    'Фільтр звітів не застосовано: немає історичних дат звітів.',
  weekly_loss_approximation:
    'Тижневий ліміт збитку в бектесті знімається з початком наступного тижня, ' +
    'а в живій торгівлі — лише вручну.',
};
