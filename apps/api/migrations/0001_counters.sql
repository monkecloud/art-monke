-- This app has a Postgres database to itself, one per environment, so public is its own.
CREATE TABLE IF NOT EXISTS counters (
    name  TEXT PRIMARY KEY,
    value BIGINT NOT NULL DEFAULT 0
);
