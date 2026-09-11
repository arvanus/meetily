-- Palette key understood by the frontend (e.g. 'blue'); NULL renders gray
ALTER TABLE tags ADD COLUMN color TEXT;

-- Optional hint sent to the summary LLM when it picks tags for a meeting
ALTER TABLE tags ADD COLUMN description TEXT;

-- When enabled, every summary run replaces the meeting's tags with the ones the LLM picks
ALTER TABLE settings ADD COLUMN autoTagMeetings INTEGER NOT NULL DEFAULT 0;
