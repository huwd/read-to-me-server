-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The fetched source each item is made from (ADR-0008: items reference source keys).
-- All null until the fetch stage completes.

ALTER TABLE items ADD COLUMN source_key TEXT;
ALTER TABLE items ADD COLUMN source_final_url TEXT;
ALTER TABLE items ADD COLUMN source_kind TEXT CHECK (source_kind IN ('html', 'text'));
ALTER TABLE items ADD COLUMN source_charset TEXT;
ALTER TABLE items ADD COLUMN source_size INTEGER;
