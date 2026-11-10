-- Local API tokens. Only a SHA-256 hash of the token is stored.
CREATE TABLE tokens (
    id                TEXT PRIMARY KEY,
    label             TEXT NOT NULL,
    token_hash        TEXT NOT NULL UNIQUE,
    display_prefix    TEXT NOT NULL,
    allowed_providers TEXT,          -- JSON array, NULL = all
    allowed_tasks     TEXT,          -- JSON array, NULL = all
    spend_cap_usd     REAL,          -- NULL = uncapped
    created_at        TEXT NOT NULL,
    last_used_at      TEXT,
    revoked_at        TEXT
);

-- One row per call. Never stores request content or provider keys.
CREATE TABLE usage (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           TEXT NOT NULL,
    token_id     TEXT NOT NULL,
    token_label  TEXT NOT NULL,
    model        TEXT NOT NULL,
    provider     TEXT NOT NULL,
    task         TEXT NOT NULL,
    units        REAL NOT NULL,
    unit         TEXT NOT NULL,
    est_cost_usd REAL NOT NULL,
    status       TEXT NOT NULL
);

CREATE INDEX usage_provider_ts ON usage (provider, ts);
CREATE INDEX usage_token ON usage (token_id);
