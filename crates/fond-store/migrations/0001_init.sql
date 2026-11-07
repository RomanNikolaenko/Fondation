CREATE TABLE bars (
    symbol TEXT NOT NULL,
    date   TEXT NOT NULL,
    open   REAL NOT NULL,
    high   REAL NOT NULL,
    low    REAL NOT NULL,
    close  REAL NOT NULL,
    volume REAL NOT NULL,
    PRIMARY KEY (symbol, date)
);

CREATE TABLE calendar_sessions (
    date  TEXT PRIMARY KEY,
    open  TEXT NOT NULL,
    close TEXT NOT NULL
);

CREATE TABLE universe (
    symbol   TEXT PRIMARY KEY,
    name     TEXT NOT NULL,
    sector   TEXT NOT NULL,
    source   TEXT NOT NULL,
    excluded INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE assets (
    symbol     TEXT PRIMARY KEY,
    tradable   INTEGER NOT NULL,
    active     INTEGER NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE scan_results (
    date   TEXT NOT NULL,
    symbol TEXT NOT NULL,
    passed INTEGER NOT NULL,
    reason TEXT,
    PRIMARY KEY (date, symbol)
);

CREATE TABLE cycle_runs (
    trading_date TEXT NOT NULL,
    step         TEXT NOT NULL,
    status       TEXT NOT NULL,
    started_at   TEXT NOT NULL,
    finished_at  TEXT,
    error        TEXT,
    PRIMARY KEY (trading_date, step)
);

CREATE TABLE engine_state (
    id                INTEGER PRIMARY KEY CHECK (id = 1),
    mode              TEXT NOT NULL,
    reason            TEXT,
    updated_at        TEXT NOT NULL,
    last_reconcile_at TEXT
);

CREATE TABLE events (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    at        TEXT NOT NULL,
    kind      TEXT NOT NULL,
    symbol    TEXT,
    intent_id TEXT,
    message   TEXT NOT NULL,
    data      TEXT NOT NULL
);
CREATE INDEX idx_events_at ON events (at);

CREATE TRIGGER events_no_update BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;

CREATE TRIGGER events_no_delete BEFORE DELETE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;

CREATE TABLE signals (
    id              TEXT PRIMARY KEY,
    signal_date     TEXT NOT NULL,
    strategy        TEXT NOT NULL,
    symbol          TEXT NOT NULL,
    reference_price REAL NOT NULL,
    stop_price      REAL NOT NULL,
    take_profit     REAL,
    strength        REAL NOT NULL,
    explanation     TEXT NOT NULL,
    status          TEXT NOT NULL,
    reject_reason   TEXT
);

CREATE TABLE earnings_checks (
    symbol        TEXT NOT NULL,
    signal_date   TEXT NOT NULL,
    next_earnings TEXT,
    blocked       INTEGER NOT NULL,
    source_ok     INTEGER NOT NULL,
    PRIMARY KEY (symbol, signal_date)
);

CREATE TABLE news_verdicts (
    symbol      TEXT NOT NULL,
    signal_date TEXT NOT NULL,
    verdict     TEXT NOT NULL,
    reasons     TEXT NOT NULL,
    key_events  TEXT NOT NULL,
    model       TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (symbol, signal_date)
);

CREATE TABLE llm_usage (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    at            TEXT NOT NULL,
    model         TEXT NOT NULL,
    input_tokens  INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cost_usd      REAL NOT NULL
);

CREATE TABLE intents (
    id              TEXT PRIMARY KEY,
    session_date    TEXT NOT NULL,
    signal_date     TEXT NOT NULL,
    symbol          TEXT NOT NULL,
    strategy        TEXT NOT NULL,
    kind            TEXT NOT NULL,
    payload         TEXT NOT NULL,
    status          TEXT NOT NULL,
    explanation     TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    broker_order_id TEXT,
    error           TEXT
);
CREATE INDEX idx_intents_session_status ON intents (session_date, status);

CREATE TABLE orders (
    id              TEXT PRIMARY KEY,
    client_order_id TEXT NOT NULL,
    symbol          TEXT NOT NULL,
    payload         TEXT NOT NULL,
    status          TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE TABLE positions (
    symbol     TEXT PRIMARY KEY,
    payload    TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE equity_snapshots (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    at           TEXT NOT NULL,
    kind         TEXT NOT NULL,
    session_date TEXT NOT NULL,
    equity       REAL NOT NULL,
    cash         REAL NOT NULL
);

CREATE TABLE backtest_runs (
    id         TEXT PRIMARY KEY,
    created_at TEXT NOT NULL,
    status     TEXT NOT NULL,
    request    TEXT NOT NULL,
    result     TEXT,
    error      TEXT
);
