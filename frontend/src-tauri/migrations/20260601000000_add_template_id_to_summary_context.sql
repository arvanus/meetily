-- Persist the per-meeting summary template selection so the template menu
-- restores the chosen option instead of resetting to default on app restart.
-- Nullable: a meeting without a saved selection falls back to the UI default.

ALTER TABLE meeting_summary_context ADD COLUMN template_id TEXT;
