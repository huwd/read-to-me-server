-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The synthesised speech plan for each item (FR-TTS-04). Null until the synthesise
-- stage completes.

ALTER TABLE items ADD COLUMN speech_plan_key TEXT;
ALTER TABLE items ADD COLUMN speech_duration_ms INTEGER;
ALTER TABLE items ADD COLUMN speech_segments INTEGER;
