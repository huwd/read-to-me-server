-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Encoded episodes, publication times, and private feeds (ADR-0009, FR-FEED-01/05).

ALTER TABLE items ADD COLUMN episode_key TEXT;
ALTER TABLE items ADD COLUMN episode_bytes INTEGER;
ALTER TABLE items ADD COLUMN episode_duration_ms INTEGER;
ALTER TABLE items ADD COLUMN published_at INTEGER;

CREATE INDEX items_published ON items (user_id, published_at) WHERE published_at IS NOT NULL;

-- One feed per user. The capability token is stored only as its hash.
CREATE TABLE feeds (
    user_id    TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    guid       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    rotated_at INTEGER NOT NULL
) STRICT;
