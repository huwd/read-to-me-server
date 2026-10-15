-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Items and the job queue (ADR-0003). Times are milliseconds since the Unix epoch.

CREATE TABLE items (
    id              TEXT PRIMARY KEY,             -- UUIDv7
    user_id         TEXT NOT NULL,
    source_url      TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN ('pending', 'published', 'failed')),
    -- The stage waited for (pending) or the stage that failed (failed).
    stage           TEXT CHECK (stage IN ('fetch', 'extract', 'script', 'synthesise', 'encode', 'publish')),
    failure_code    TEXT,
    failure_message TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    CHECK ((state = 'published') = (stage IS NULL)),
    CHECK ((state = 'failed') = (failure_code IS NOT NULL))
) STRICT;

CREATE INDEX items_by_user ON items (user_id, created_at);

CREATE TABLE jobs (
    id           INTEGER PRIMARY KEY,
    item_id      TEXT NOT NULL UNIQUE REFERENCES items (id) ON DELETE CASCADE,
    user_id      TEXT NOT NULL,               -- the fairness key
    stage        TEXT NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0,
    run_after    INTEGER NOT NULL,
    locked_by    TEXT,
    locked_until INTEGER,
    last_error   TEXT
) STRICT;

CREATE INDEX jobs_runnable ON jobs (run_after);

-- When each user last had a job claimed, for round-robin claiming (NFR-S2).
CREATE TABLE queue_turns (
    user_id         TEXT PRIMARY KEY,
    last_claimed_at INTEGER NOT NULL
) STRICT;
