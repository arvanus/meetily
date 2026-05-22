-- Migration: per-meeting AI summary context (textarea + file attachments).
-- Separate from meeting_notes (which stores recording-time notes).

CREATE TABLE IF NOT EXISTS meeting_summary_context (
    meeting_id TEXT PRIMARY KEY NOT NULL,
    context_prompt TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL,
    FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS meeting_context_attachments (
    id TEXT PRIMARY KEY NOT NULL,
    meeting_id TEXT NOT NULL,
    original_path TEXT NOT NULL,
    stored_filename TEXT NOT NULL,
    display_name TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    truncated INTEGER NOT NULL DEFAULT 0,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_meeting_context_attachments_meeting_id
    ON meeting_context_attachments(meeting_id);
