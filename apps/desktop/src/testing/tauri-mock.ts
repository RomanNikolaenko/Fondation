import { vi } from 'vitest';

/**
 * Хелпери для тестів UI: підміна Tauri `invoke`/`listen`.
 *
 * У spec-файлі спершу:

 * ```ts
 * const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
 * vi.mock('@tauri-apps/api/core', () => ({ invoke }));
 * ```
 *
 * а далі — `mockInvoke(invoke, { get_status: () => status })`.
 */
export type InvokeMock = ReturnType<typeof vi.fn>;

export function mockInvoke(
  invoke: InvokeMock,
  handlers: Record<string, (args?: Record<string, unknown>) => unknown>,
): void {
  invoke.mockImplementation((command: string, args?: Record<string, unknown>) => {
    const handler = handlers[command];
    if (!handler) {
      return Promise.reject(new Error(`Неочікувана команда: ${command}`));
    }
    try {
      return Promise.resolve(handler(args));
    } catch (error) {
      return Promise.reject(error);
    }
  });
}

export function mockListen(): ReturnType<typeof vi.fn> {
  return vi.fn(async () => () => {});
}
