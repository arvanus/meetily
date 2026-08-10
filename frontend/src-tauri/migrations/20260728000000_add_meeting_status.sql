-- Track the recording lifecycle on the meeting row itself.
--
-- Recordings (UI and CLI alike) now INSERT their meeting when they START, with
-- status 'recording', and flip it to 'completed' on a clean stop. A row left at
-- 'recording' therefore means the process died before finishing, which is what
-- the startup recovery flow looks for.
--
-- Existing rows were all written by the old save-on-stop path, so by definition
-- they are complete: the DEFAULT backfills them correctly.
ALTER TABLE meetings ADD COLUMN status TEXT NOT NULL DEFAULT 'completed';

-- Recovery scans filter on status; meetings lists filter it out.
CREATE INDEX IF NOT EXISTS idx_meetings_status ON meetings(status);
