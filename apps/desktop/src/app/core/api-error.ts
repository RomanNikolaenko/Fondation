/** Формат помилок команд Tauri (`ClientError` у `engine_client.rs`). */
export type ClientError =
  | { kind: 'unreachable'; message: string }
  | { kind: 'http'; status: number; message: string; details: string[] }
  | { kind: 'decode'; message: string };

export function isClientError(value: unknown): value is ClientError {
  if (typeof value !== 'object' || value === null || !('kind' in value)) {
    return false;
  }
  const kind = (value as { kind: unknown }).kind;
  return kind === 'unreachable' || kind === 'http' || kind === 'decode';
}

/** Український текст для будь-якої помилки команди. */
export function errorMessage(error: unknown): string {
  if (isClientError(error)) {
    switch (error.kind) {
      case 'unreachable':
        return `Рушій недоступний: ${error.message}`;
      case 'http': {
        const details = error.details.length > 0 ? ` (${error.details.join('; ')})` : '';
        return `Помилка ${error.status}: ${error.message}${details}`;
      }
      case 'decode':
        return `Некоректна відповідь рушія: ${error.message}`;
    }
  }
  if (error instanceof Error) {
    return error.message;
  }
  if (typeof error === 'string' && error.trim() !== '') {
    return error;
  }
  return 'Невідома помилка';
}
