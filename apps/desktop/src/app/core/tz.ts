const ET = 'America/New_York';
const KYIV = 'Europe/Kyiv';

function partsInZone(instant: Date, timeZone: string): number {
  const parts = new Intl.DateTimeFormat('en-GB', {
    timeZone,
    hourCycle: 'h23',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  }).formatToParts(instant);
  const value = (type: string): number => {
    const part = parts.find((p) => p.type === type);
    return part ? Number(part.value) : 0;
  };
  return Date.UTC(
    value('year'),
    value('month') - 1,
    value('day'),
    value('hour'),
    value('minute'),
    value('second'),
  );
}

/** Зсув зони в мс для конкретного моменту (додатний на схід від UTC). */
function zoneOffsetMs(instantMs: number, timeZone: string): number {
  return partsInZone(new Date(instantMs), timeZone) - instantMs;
}

/**
 * Настінний час Нью-Йорка (`YYYY-MM-DD`, `HH:MM` або `HH:MM:SS`) → UTC.
 * Враховує перехід на літній/зимовий час.
 */
export function etWallTimeToUtc(date: string, time: string): Date {
  const [year, month, day] = date.split('-').map(Number);
  const [hour, minute, second = 0] = time.split(':').map(Number);
  if ([year, month, day, hour, minute, second].some((v) => !Number.isFinite(v))) {
    throw new Error(`Некоректний час: ${date} ${time}`);
  }

  const naive = Date.UTC(year, month - 1, day, hour, minute, second);
  // Два проходи: перший — за наближеним зсувом, другий — за уточненим.
  let offset = zoneOffsetMs(naive, ET);
  offset = zoneOffsetMs(naive - offset, ET);
  return new Date(naive - offset);
}

function clockInZone(instant: Date, timeZone: string): string {
  return new Intl.DateTimeFormat('en-GB', {
    timeZone,
    hourCycle: 'h23',
    hour: '2-digit',
    minute: '2-digit',
  }).format(instant);
}

/** `'2026-11-05T14:00:00Z'` → `'09:00 ET · 16:00 Київ'`. */
export function formatEtKyiv(iso: string): string {
  const instant = new Date(iso);
  if (Number.isNaN(instant.getTime())) {
    return '—';
  }
  return `${clockInZone(instant, ET)} ET · ${clockInZone(instant, KYIV)} Київ`;
}

/** Поточна дата за Нью-Йорком у форматі `YYYY-MM-DD`. */
export function todayEt(now: Date = new Date()): string {
  const parts = new Intl.DateTimeFormat('en-GB', {
    timeZone: ET,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
  }).formatToParts(now);
  const value = (type: string): string =>
    parts.find((p) => p.type === type)?.value ?? '';
  return `${value('year')}-${value('month')}-${value('day')}`;
}
