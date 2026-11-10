CREATE TABLE ai_proposals (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    at              TEXT NOT NULL,
    run_date        TEXT NOT NULL,
    symbol          TEXT NOT NULL,
    action          TEXT NOT NULL,
    reference_price REAL NOT NULL,
    stop_price      REAL NOT NULL,
    take_profit     REAL,
    strength        REAL NOT NULL,
    explanation     TEXT NOT NULL,
    key_reasons     TEXT NOT NULL,
    model           TEXT NOT NULL,
    input_tokens    INTEGER NOT NULL,
    output_tokens   INTEGER NOT NULL,
    cost_usd        REAL NOT NULL,
    status          TEXT NOT NULL,
    reject_reason   TEXT
);
CREATE INDEX idx_ai_proposals_at ON ai_proposals (at);
