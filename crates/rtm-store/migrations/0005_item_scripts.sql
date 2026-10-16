-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The narration script each item will be synthesised from (FR-TTS-01). Null until the
-- script stage completes.

ALTER TABLE items ADD COLUMN script_key TEXT;
ALTER TABLE items ADD COLUMN script_words INTEGER;
