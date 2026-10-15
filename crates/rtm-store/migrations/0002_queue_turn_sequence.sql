-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Round-robin turns become a strictly increasing sequence: timestamps tie when several
-- claims land in the same millisecond, which broke the alternation between users.
-- Existing values (millisecond times) are already increasing, so they carry over as-is.

ALTER TABLE queue_turns RENAME COLUMN last_claimed_at TO turn;
