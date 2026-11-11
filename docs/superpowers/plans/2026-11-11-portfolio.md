# План реалізації: Портфель і ребалансування

Специфікація: `docs/superpowers/specs/2026-11-11-portfolio-design.md`.
Кожен крок — зелений коміт: тести крейта + `pnpm check` перед комітом.
Відповіді українською; коментарі в коді українською.

## Крок 1. Доменні типи і математика в `fond-core`

- [ ] `IntentTag::Rebalance` (у `ids.rs`, рядок в id — `rebalance`)
- [ ] `IntentKind::Rebalance { side, qty, limit_price }` (у `intent.rs`)
- [ ] Модуль `rebalance.rs`: `RebalanceDelta`, `compute_deltas(targets,
      positions, prices, equity, drift, min_order_usd, max_gap)` — продажі
      перед купівлями, пороги drift/min_order_usd, обрізання купівель під
      кеш, пропуски без ціни
- [ ] Тести: пороги, обрізання кешу, продаж не більше наявного, сортування,
      відсутність барав
- [ ] Коміт: `feat(core): rebalance intents and delta math`

## Крок 2. Конфіг у `fond-api`

- [ ] `PortfolioConfig { enabled, rebalance, targets: BTreeMap<…>, drift,
      min_order_usd }` + `Default` + `#[serde(default)]` + ts-rs
- [ ] Валідація в `EngineConfig::validate` (ваги, сума ≤ 1, drift, rebalance,
      enabled → not off)
- [ ] Перегенерація DTO (`PortfolioConfig.ts`, `RebalanceMode.ts` за потреби)
- [ ] Тести валідації
- [ ] Коміт: `feat(api): portfolio config section`

## Крок 3. Прохід ребалансу в `fond-engine`

- [ ] Визначення «перший торговий день місяця/кварталу» за календарем
      (чиста функція з тестами)
- [ ] `portfolio_pass` у вечірньому аналізі після ризик-проходу: дельти →
      наміри (`PendingApproval`/`Approved`), журнал-підсумок; коли не день
      ребалансу — тихий пропуск
- [ ] Rизик: купівлі в межах кешу (власна перевірка проходу, не
      `risk::evaluate` — наміри не мають стопів); `max_orders_per_day`
      враховує ребаланс-наміри
- [ ] Submit: порядок черги з продажами ребалансу перед купівлями,
      `build_rebalance_order` (Limit Day без стоп-леги)
- [ ] Reconcile: `Rebalance` у кроці 1; купівля → tracked-позиція стратегії
      `portfolio` без стопа; `check_stop` пропускає re-protect для неї;
      продаж → зменшення qty (0 → закрито)
- [ ] Тести: день місяця/кварталу, створення намірів, черга submit,
      звірка купівлі/продажу
- [ ] Коміт: `feat(engine): portfolio rebalance pass`

## Крок 4. Ручний запуск і REST

- [ ] Mutex `portfolio` у `Jobs`; `Engine::run_manual_rebalance()`
      (під `job_lock`, з журналюванням помилки)
- [ ] `POST /portfolio/rebalance/run` → 202/409; `RebalanceRunQueuedDto`
- [ ] Тести API: 202 → наміри створено, 409 конфлікт
- [ ] Перегенерація DTO
- [ ] Коміт: `feat(api): manual portfolio rebalance`

## Крок 5. Екран «Портфель» у `apps/desktop`

- [ ] Tauri-команди `run_portfolio_rebalance`; `EngineService` метод
- [ ] Екран `features/portfolio`: таблиця ціль/факт/відхилення/дельта,
      редактор цілей (додати/видалити, вага %, сума ≤ 100%), кнопка
      «Ребалансувати зараз» (202-повідомлення, 409-помилка), живе
      оновлення через журнал-події
- [ ] Маршрут + nav «Портфель» + i18n; спеки оновлені
- [ ] Тести vitest: рендер таблиці, редагування цілей, run → повідомлення,
      конфлікт 409
- [ ] Коміт: `feat(desktop): portfolio screen`

## Крок 6. Документи

- [ ] README: розділ «Портфель і ребалансування»
- [ ] `docs/acceptance/part3.md` — чекліст приймання (paper: ціль →
      ребаланс → схвалення → ордер; пороги; ручний запуск; 409)
- [ ] Коміт: `docs: portfolio readme and acceptance`

## Крок 7. Приймання (ручне)

- [ ] Прогін чеклісту part3 на paper-ключах, позначки `[x]`
- [ ] Коміт: `docs: part 3 acceptance checklist`

## Ключові рішення (зафіксовані)

1. Ребаланс — всередині вечірнього кроку `Analysis`, без нового кроку
   розкладу; ручний запуск — окремий ендпойнт під `job_lock`.
2. Намір `Rebalance` (buy/sell Limit Day, без стоп-леги) — новий вид;
   продажи продаються перед купівлями.
3. Позиції від ребалансу мають стратегію `portfolio` і без стоп-леги;
   звірка не намагається відновити їм стоп.
4. Математика дельт — чиста функція в `fond-core`; рушій тільки застосовує.
5. Бек-тест ребалансу — поза обсягом v1.
