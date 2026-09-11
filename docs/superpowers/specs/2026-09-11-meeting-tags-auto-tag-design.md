# Meeting tags: management UI, colors and AI auto-tagging

## Context

Meeting tags (M:N, `tags` + `meeting_tags`) came from the piper5ul fork: tags can be
created and assigned from a popover in the meeting summary header, and the sidebar can
filter by tag. There is no place to rename, delete or color tags, and tags are never
assigned automatically.

## Goals

1. Manage tags in Settings: create, rename, delete, color, optional description.
2. Show each meeting's tags in the sidebar row, when it has any.
3. Let the summary LLM assign tags when a summary is generated, in the app and in the CLI.

## Decisions

- The AI only picks from tags that already exist. It never creates tags.
- Every summary run replaces the meeting's tags with the AI result (manual tags included).
- Auto-tagging is a global setting, off by default, stored in the `settings` table so the
  CLI can read it. The CLI can override it per run with `--auto-tag` / `--no-auto-tag`.
- Each tag has an optional description that is sent to the AI with the name.
- Colors come from a fixed palette of 10 keys (`gray`, `red`, ...). The key is stored,
  the frontend maps it to Tailwind classes. Unknown or missing keys render gray.
- Sidebar rows show up to 2 tag chips plus `+N` with a tooltip listing the rest.

## Data

New migration `20260911000001_add_tag_color_description_auto_tag.sql` (the tags migration
is already applied on existing databases, so it is not edited):

- `tags.color TEXT`, `tags.description TEXT`
- `settings.autoTagMeetings INTEGER NOT NULL DEFAULT 0`

`TagsRepository` gains `update_tag` (rejects a name already used by another tag,
case-insensitive), `delete_tag` (removes links and the tag in one transaction) and
`list_meeting_tag_links` (every meeting/tag pair, used to attach tags to the meeting list
in one query). `api_get_meetings` and `api_get_meetings_for_tag` return `tags` per meeting.

## Auto-tagging

`summary/auto_tag.rs`:

- `build_prompts(tags, title, summary)`: system prompt with the tag list (`name: description`)
  asking for `{"tags": [...]}`; user prompt with the meeting title and the final summary.
  The summary is used instead of the transcript because it is short and fits the context of
  local models that chunk transcripts.
- `parse_tag_names(response)`: strips thinking blocks, reads the first JSON object, falls
  back to a bare JSON array. Returns `None` when nothing parses.
- `resolve_tag_ids(names, tags)`: trimmed, case-insensitive match, deduplicated, unknown
  names dropped.
- `auto_tag_meeting(...)`: runs the LLM call through `llm_client::generate_summary` and
  replaces the meeting tags.

`SummaryService::process_transcript_background` takes `auto_tag: Option<bool>` (`None` =
use the setting). After the summary is generated and the title extracted, and before the
process is marked completed, it runs `auto_tag_meeting`, so the UI sees the tags as soon as
polling reports completion. Tags are left unchanged when:

- auto-tagging is disabled, or no tags exist, or the summary is empty;
- the LLM call fails, is cancelled, or the reply cannot be parsed;
- the reply names only tags that do not exist.

A valid empty list clears the meeting tags. Auto-tag failures are logged and never fail
the summary.

The CLI (`summarize`, `record --summarize`, `retranscribe --summarize`) goes through the
same function and prints the resulting tags.

## UI

- Settings > Tags: auto-tag switch (with a note that it replaces the meeting's tags), tag
  list with chip, description and meeting count, create/edit dialog (name, color palette,
  description), delete confirmation showing how many meetings use the tag.
- `lib/tagColors.ts` palette and `components/MeetingTags/TagChip.tsx`, reused by the
  sidebar rows, the sidebar tag filter and the tag picker.
- When a summary completes, `useSummaryGeneration` dispatches `meetily-tags-updated`, which
  refreshes the sidebar and the meeting tags button.

## Out of scope

Tags in `summary.md`, CLI commands to manage tags, merging tags.

## Testing

- Rust unit tests: tag update/rename conflict/delete, meeting/tag links; prompt parsing
  (plain JSON, fenced JSON, thinking blocks, bare array, garbage) and name resolution.
- CLI argument tests for the new flags.
- `tsc --noEmit`, `cargo test` for the touched modules, and an app build.
