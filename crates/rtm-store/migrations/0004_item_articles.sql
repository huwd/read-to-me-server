-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The extracted article each item narrates (ADR-0006, FR-EXT-04). All null until the
-- extract stage completes.

ALTER TABLE items ADD COLUMN article_key TEXT;
ALTER TABLE items ADD COLUMN extractor TEXT;
ALTER TABLE items ADD COLUMN title TEXT;
ALTER TABLE items ADD COLUMN title_derived INTEGER CHECK (title_derived IN (0, 1));
ALTER TABLE items ADD COLUMN article_words INTEGER;
