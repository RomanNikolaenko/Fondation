# План реалізації: Портфель і ребалансування

Специфікація: `docs/superpowers/specs/2026-11-11-portfolio-design.md`.
Кожен крок — зелений коміт: тести крейта + `pnpm check` перед комітом.
Відповіді українською; коментарі в коді українською.

## Крок 1. Доменні типи і математика в `fond-core`

- [x] Модуль `rebalance.rs`: `RebalanceDelta`, `SkipReason`/`Skipped`,
      `RebalancePlan`, `compute_deltas(targets, positions, prices, equity,
      cash, params)` — продажі перед купівлями, пороги drift/min_order_usd,
      обрізання купівель під кеш, пропуски без ціни, продаж не більше
      наявного (`PORTFOLIO_STRATEGY = "portfolio"`)
- [x] Тести: пороги, обрізання кешу, сортування, відсутність барав, порожній
      план (7 тестів, 73 зелені в крейті)
- [ ] `IntentTag::Rebalance` + `IntentKind::Rebalance` — перенесено у Крок 3
      (додавання варіанта енуму ламає exhaustive match у рушії; типи йдуть
      разом із підтримкою конвеєра, щоб кожен коміт залишався зеленим)
- [x] Коміт: `feat(core): rebalance delta math`

## Крок 2. Конфіг у `fond-api`

- [x] `PortfolioConfig { enabled, rebalance: RebalanceMode, targets:
      BTreeMap<String, f64>, drift, min_order_usd }` + `Default` +
      `#[serde(default)]` + ts-rs; `RebalanceMode = off | monthly | quarterly`
- [x] Валідація в `EngineConfig::validate` (ваги (0,1], сума ≤ 1,
      drift (0, 0.5], min_order_usd ≥ 1, enabled → not off)
- [x] Перегенерація DTO (`PortfolioConfig.ts`, `RebalanceMode.ts`); `portfolio`
      у TS-mock-конфізах `strategies.spec.ts` і `settings.spec.ts`
- [x] Тести валідації (18 у крейті, workspace зібрано)
- [x] Коміт: `feat(api): portfolio config section`

## Крок 3. Прохід ребалансу в `fond-engine`

- [x] «Перший торговий день місяця/кварталу» за календарем — чиста функція
      `is_rebalance_day(session, mode, cal)` у `cycle/portfolio.rs`
      (попередня сесія з іншого місяця/кварталу; без попередньої сесії —
      не день ребалансу)
- [x] `portfolio_pass` у вечірньому аналізі після AI-проходу: дельти →
      наміри `PendingApproval`/`Approved` (за режимом виконання), пояснення
      «докупити/продати до цілі N% (±$X)», журнал-підсумок, `IntentsPending`;
      позарозкладний виклик — тихий пропуск. Тікери цілей зі swing-позиціями
      або живим наміром сесії — пропуск із журналом; кеш = брокерський мінус
      зарезервований живими входами/ребалансами сесії;
      `AnalysisSummary.rebalances_created`
- [x] Купівлі в межах вільного кешу (власна перевірка проходу); розклад
      на день сесії; `orders_planned` враховує ребаланс-наміри; kill
      `settle_submitted` включає Rebalance
- [x] Submit: черга MoveStop(0) < Exit(1) < Rebalance Sell(2, без gate) <
      Rebalance Buy(3, gate+ліміт як у входів) < Entry(4);
      `build_rebalance_order` (Limit Day Simple, side за наміром);
      `send_rebalance` (price sanity + submit_idempotent); поле `rebalances`
      у `SubmitSummary` і finish-повідомленні
- [x] Reconcile: `Rebalance` у кроці 1 (`rebalance_order`); купівля →
      позиція стратегії `portfolio` без стопа (merge у наявну
      portfolio-позицію; swing-конфлікт — журнал Error, намір Filled);
      продаж → зменшення qty (0 → remove + cancel_open_sells), без
      reprotect; крок 2 для portfolio-позицій: flat → close_flat,
      інакше пропуск; `ReconcileReport.rebalances_filled` + summarize
- [x] Тести: день місяця/кварталу/року, створення намірів (3 стенди),
      черга submit (продаж перед купівлею, Limit Day Simple, price
      sanity), звірка купівлі/продажу/не-виконано (210 тестів крейта,
      workspace зелений)
- [x] Коміт: `feat(engine): portfolio rebalance pass`

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
