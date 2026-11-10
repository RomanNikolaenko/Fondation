# План реалізації: AI-аналітик (частина 2)

Дата: 2026-11-09. Спека: `docs/superpowers/specs/2026-11-09-ai-analyst-design.md`.

Працюємо по одному кроку, кожен коміт — зелений `pnpm check`.

## Крок 1. Типи пропозиції у `fond-core`

- [x] `AiAction { Buy, Exit }`, `AiProposal` у `fond-core/src/ai.rs`; `#[cfg_attr(feature = "ts", derive(ts_rs::TS))]`
- [x] Тест: серіалізація тегів (`snake_case`), валідність `strength` 0..1
- [x] Коміт: `feat(core): ai proposal types`

## Крок 2. Адаптер `claude_strategist` у `fond-adapters`

- [x] `AiStrategist`-клієнт на базі `claude.rs`: системний промпт, defang `<context>`,
      JSON-схема відповіді, парсинг `AiProposal[]`, rate-limit, retry
- [x] Тести wiremock: ok-відповідь, невалідний JSON, 429/5xx, таймаут
- [x] Коміт: `feat(adapters): claude ai strategist`

## Крок 3. Сховище пропозицій у `fond-store`

- [x] Міграція: `ai_proposals`
- [x] Репозиторій: insert, list за період, update статусу
- [x] Тест: insert/list-раундтрип, статуси, сортування
- [x] Коміт: `feat(store): ai proposals table`

## Крок 4. Джоб `ai_review` у `fond-engine`

- [x] Конфіг: `AiReviewConfig` (enabled, model, effort, context_bars, max_proposals, allocation, lookback_days) + валідація
- [x] Контекст-білдер: бари й скан з того самого вечірнього аналізу (AI викликається в кінці Run::execute)
- [x] Виклик стратегіста послідовно; бюджет спільний з news-фільтром (`budget_left`)
- [x] Запис пропозицій → risk::evaluate → Intent → схвалення/авто; фільтри звітів/новин для AI пропущені
- [x] Події в журнал (journal + ws): пропозиція / санітарне відхилення / помилка стратегіста / пропуск бюджету
- [x] Тести (5): пропозиція → намір+запис IntentCreated+llm_usage; вичерпаний бюджет; кривий стоп; помилка стратегіста; контекст
- [x] Коміт: `feat(engine): ai analyst pass in analysis, budget-guarded, proposal journal`

## Крок 5. REST/DTO у `fond-api`

- [ ] `AiProposalDto`, `GET /proposals/ai?limit=`, `POST /proposals/ai/run`
- [ ] Џереєрація TypeScript (DTO-снапшот узгоджений)
- [ ] Коміт: `feat(api): ai analyst endpoints`

## Крок 6. Екран «AI-аналіз» в `apps/desktop`

- [ ] Сервиз: список пропозицій, кнопка «Запустити аналіз» (confirm у «з підтвердженням»)
- [ ] UI: картка пропозиції — тікер, дія, стоп/таргет, впевненість (badge),
      пояснення українською, статус конвеєру, кнопки «Схвалити / Відхилити»
- [ ] Порожні стани: «немає ключа», «немає бюджету», «немає пропозицій»
- [ ] Тести vitest: рендер списку, run → POST, схвалення → UI-поздоровлення
- [ ] oxfmt/stylelint чисті
- [ ] Коміт: `feat(desktop): ai analysis screen`

## Крок 7. Налаштування і README

- [ ] Розклад AI-аналітика в «Налаштуваннях» (ET, подібно до основного розкладу) + ліміт пропозицій за запуск
- [ ] README: розділ «AI-аналітик»
- [ ] Коміт: `docs: ai analyst settings and readme`

## Крок 8. Приймання і пакування

- [ ] Ручне приймання: запуск джоба на paper-ключах, пропозиція → схвалення → ордер
      на Alpaca paper; `docs/acceptance/part2.md`
- [ ] `pnpm package` → NSIS-інсталятор
- [ ] Коміт: `docs: part 2 acceptance checklist`

## Ключові рішення (зафіксовані)

1. AI не замінює `Strategy::evaluate_entry` — окремий джоб `ai_review`.
2. Новинний фільтр для AI-пропозицій не викликається (AI вже бачив новини).
3. Виклики LLM — через той самий rate-limiter і бюджет, що й news-judge.
4. Схвалення і ризик-ліміти — без змін: ордери лише через існуючий конвеєр.
