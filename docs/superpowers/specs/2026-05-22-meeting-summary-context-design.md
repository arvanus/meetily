# Meeting Summary Context: Persistence + File Attachments

**Date:** 2026-05-22
**Status:** Draft
**Author:** Lucas Rubian Schatz
**Related issues (upstream):** [#235](https://github.com/Zackriya-Solutions/meetily/issues/235) (loosely)

## 1. Problem

The "Add context for AI summary" textarea in the meeting details page
(`frontend/src/components/MeetingDetails/TranscriptPanel.tsx:103-108`) currently:

1. **Is not persistent.** Held only in React state
   (`page-content.tsx:57`, `useState<string>('')`). Lost on page navigation,
   reload, or regeneration.
2. **Accepts text only.** Users frequently want to attach short documents
   (briefs, agendas, glossaries, transcripts of related prior meetings) as
   context but have no way to include them short of pasting their contents.

The text *is* sent to the LLM (verified at `processor.rs:364-368` — appended
inside `<user_context>` tags), so the underlying pipe works. The gap is
persistence and richer input.

## 2. Goals

- Persist the free-form context text per meeting; survive reload, regeneration,
  and app restart.
- Allow attaching text-based files (`.txt`, `.md`, source code, JSON/YAML/CSV,
  etc.) as additional context.
- Inject attachment contents into the LLM prompt in a clearly delimited block.
- Copy attachments into the meeting folder so they remain available even if
  the source file is moved or deleted.
- Apply sane size limits with visible feedback when files are truncated.

## 3. Non-goals

- **Touching `Template.context`** (the optional context field on summary
  templates, added by commit `b6bc8f7`). That field is **separate from this
  feature**: it lives on the template (not per-meeting), is injected into the
  *system prompt* as `**ADDITIONAL CONTEXT (provided by user):**`, and is
  defined when the template is created. This spec only persists the
  per-meeting *user prompt* context (the textarea). Both mechanisms coexist
  unchanged in the final prompt — `Template.context` in the system message,
  `meeting_summary_context.context_prompt` + attachments in the user message.
- Binary/document formats (PDF, DOCX, images). Text-only for v1.
- Provider-native attachment APIs (Claude PDF upload, OpenAI files). Always
  inline text content into the prompt; this keeps behavior uniform across
  providers (Ollama, Claude, Groq, OpenRouter, built-in AI).
- Drag-to-reorder attachments. Order is insertion order; a `sort_order` column
  exists for future use but the UI does not expose reordering in v1.
- Per-attachment editing. Attachments are immutable; user removes and
  re-attaches to update.
- Token-budget calculation against the selected model's context window. Out of
  scope for v1; deferred per brainstorming decision.
- Sharing context across meetings (no "templates" of context).
- Real-time collaboration on the context field.

## 4. User-facing behavior

### 4.1 Where it lives

Same region as the existing context textarea, at the bottom of the transcript
panel. Layout:

```
┌─ (transcripts above) ──────────────────────────────┐
│  ...                                                │
├─────────────────────────────────────────────────────┤
│  📎 projeto_brief.md  ✕                             │  ← attachment chips
│  📎 timeline.txt (truncated ⚠)  ✕                   │
├─────────────────────────────────────────────────────┤
│  Add context for AI summary. For example people    │  ← existing textarea
│  involved, meeting overview, objective etc...      │
│                                                     │
│                                              [📎]   │  ← attach button
└─────────────────────────────────────────────────────┘
```

- Attachment chips wrap; clicking the chip opens the stored file in the OS
  default app. The `✕` removes the attachment (file on disk + DB row).
- Truncated files show a `⚠` icon next to the name; tooltip explains the
  truncation.
- Drag-and-drop a file into the textarea region adds it as an attachment.
- The attach button (paperclip) opens the OS file picker.

### 4.2 Persistence behavior

- The textarea autosaves with a 500 ms debounce. No save button.
- Attachments persist immediately on add/remove.
- Reload the meeting → textarea text + chips restored as they were.
- Regenerate the summary → same context + attachments used; no need to
  retype/re-attach.
- Delete the meeting → cascade deletes both DB rows and the
  `<meeting_folder>/attachments/` directory.

### 4.3 Limits

| Limit | Value | Behavior when exceeded |
|---|---|---|
| Max size per attachment | 256 KB | Truncate; mark `truncated = 1`; warn in UI |
| Max attachments per meeting | 10 | Block add; toast: "Limite de 10 anexos atingido" |
| Max total bytes across attachments | 1 MB | Block add; toast: "Limite total de 1 MB atingido" |
| Non-UTF-8/binary file | n/a | Reject on add; toast: "Arquivo binário não suportado" |

Truncation strategy: read first 256 KB, decode as UTF-8 lossy, find the last
newline within the window, cut there. If no newline, cut at byte 256 KB
(after the lossy decode pass to avoid splitting a multi-byte character).

## 5. Architecture

### 5.1 Data model

New migration `migrations/<timestamp>_add_meeting_summary_context.sql`:

```sql
-- Free-form context text the user provides for AI summary generation.
-- Separate from meeting_notes (which stores recording-time notes).
CREATE TABLE IF NOT EXISTS meeting_summary_context (
    meeting_id TEXT PRIMARY KEY NOT NULL,
    context_prompt TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL,
    FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
);

-- File attachments included as context for summary generation.
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
```

**Why two tables, not columns on `meetings`:** the existing codebase follows a
per-concern table pattern. `meetings` is intentionally minimal (4 columns +
`folder_path` from a later migration). `meeting_notes` (added by issue #389's
implementation) uses `meeting_id` as PK for the same one-row-per-meeting
pattern we adopt here.

**Why not extend `meeting_notes`:** semantically different. `meeting_notes`
stores user-visible notes captured during recording (markdown + BlockNote JSON
dual). `meeting_summary_context` stores text that influences the LLM,
invisible in the report. Coupling them would force one schema to serve two
unrelated lifecycles.

### 5.2 Filesystem layout

```
<meeting_folder>/
├── transcripts.json
├── summary.md
├── audio/
└── attachments/                  ← new
    ├── 7c3a-b1e0_brief.md
    ├── 9f12-a420_timeline.txt
    └── ...
```

- `stored_filename` format: `<short-uuid>_<sanitized-original-basename>`.
  The UUID prefix avoids collisions when two files have the same name.
- Sanitization: strip control characters; replace path separators with `_`;
  cap basename at 64 chars.
- Removed attachments delete their stored file. Orphan cleanup is not needed
  because cascading deletes handle meeting-level cleanup, and removal is the
  only other path.

### 5.3 Backend (Rust) module layout

New module `frontend/src-tauri/src/summary/context/`:

```
summary/
├── context/                         ← new
│   ├── mod.rs                       — re-exports
│   ├── commands.rs                  — tauri commands (frontend boundary)
│   ├── storage.rs                   — copy/truncate/sanitize on disk
│   ├── repository.rs                — SQLx CRUD
│   ├── types.rs                     — DTO structs (serde)
│   └── prompt_builder.rs            — render attachments into prompt block
├── processor.rs                     — modified to receive attachments
├── service.rs                       — modified to pass attachments through
├── commands.rs                      — `api_process_transcript` modified
└── ...
```

#### Tauri commands

```rust
// summary/context/commands.rs

#[tauri::command]
pub async fn api_save_summary_context(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    context_prompt: String,
) -> Result<(), String>;

#[tauri::command]
pub async fn api_get_summary_context(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<SummaryContext, String>;
// SummaryContext { context_prompt: String, attachments: Vec<AttachmentInfo> }

#[tauri::command]
pub async fn api_add_context_attachment<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    source_path: String,
) -> Result<AttachmentInfo, String>;

#[tauri::command]
pub async fn api_remove_context_attachment(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    attachment_id: String,
) -> Result<(), String>;

#[tauri::command]
pub async fn api_open_context_attachment<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    attachment_id: String,
) -> Result<(), String>;
```

`AttachmentInfo` shape (TS-facing):

```ts
{
  id: string;
  display_name: string;
  size_bytes: number;
  truncated: boolean;
  created_at: string;
}
```

#### Storage flow on add

1. Validate source path exists and is a file.
2. Open and stream-read up to 256 KB + 1 byte.
3. Decode as UTF-8. If invalid (non-text), reject.
4. If > 256 KB, truncate at last newline within window; mark `truncated`.
5. Resolve `<meeting_folder>` from `meetings.folder_path`. If null, error
   ("Meeting folder not initialized — start recording at least once").
6. Ensure `<meeting_folder>/attachments/` exists.
7. Generate `stored_filename = "{uuid}_{sanitized_basename}"`.
8. Write to disk; insert DB row; return `AttachmentInfo`.
9. On any failure after step 7, clean up the written file.

#### Prompt construction

Modify `summary/processor.rs`:

```rust
// Current (processor.rs:357-368):
final_user_prompt.push_str(&format!(
    r#"<transcript_chunks>
{}
</transcript_chunks>"#,
    content_to_summarize
));

if !custom_prompt.is_empty() {
    final_user_prompt.push_str("\n\nUser Provided Context:\n\n<user_context>\n");
    final_user_prompt.push_str(custom_prompt);
    final_user_prompt.push_str("\n</user_context>");
}

// New: insert attachments between transcript and user_context.
if !attachments.is_empty() {
    final_user_prompt.push_str("\n\n<attachments>\n");
    for a in &attachments {
        final_user_prompt.push_str(&format!(
            "<file name=\"{}\"{}>\n{}\n</file>\n",
            xml_escape(&a.display_name),
            if a.truncated { " truncated=\"true\"" } else { "" },
            a.content,
        ));
    }
    final_user_prompt.push_str("</attachments>");
}
```

Final prompt structure (user message):

```
<transcript_chunks>
  ...transcript...
</transcript_chunks>

<attachments>
  <file name="brief.md">
    ...content...
  </file>
  <file name="timeline.txt" truncated="true">
    ...content (cut)...
  </file>
</attachments>

User Provided Context:

<user_context>
  ...textarea text...
</user_context>
```

### 5.4 Frontend layout

New hook `frontend/src/hooks/meeting-details/useSummaryContext.ts`:

```ts
export function useSummaryContext(meetingId: string) {
  // - Loads context_prompt + attachments on mount via api_get_summary_context.
  // - Exposes contextPrompt, setContextPrompt (debounced save 500ms).
  // - Exposes attachments, addAttachment(path), removeAttachment(id),
  //   openAttachment(id).
  // - Manages loading/error state.
}
```

New component `frontend/src/components/MeetingDetails/ContextAttachmentsBar.tsx`:

```tsx
// Renders the chip list above the textarea + paperclip button below-right.
// Receives: attachments, onAdd, onRemove, onOpen.
```

Modifications:

- `TranscriptPanel.tsx` — wire in `ContextAttachmentsBar` above the existing
  textarea. Remove the `customPrompt`/`onPromptChange` props that come from
  page-content; instead consume `useSummaryContext` directly (or accept it as
  a prop bag if we want to keep the panel pure). Add drag-and-drop handlers
  on the bottom region.
- `page-content.tsx` — drop the local `useState` for `customPrompt`; pull
  from `useSummaryContext`. The state still flows down to `SummaryPanel`
  unchanged (it's already a prop).
- `useSummaryGeneration.ts` — drops the `customPrompt` argument from the
  `invoke('api_process_transcript', ...)` call entirely.

**Decision on payload at generation time:** the backend fetches both
`context_prompt` and `attachments` for `meeting_id` by itself, directly from
the DB. The frontend's `api_process_transcript` call drops the
`customPrompt` parameter; the Rust command reads
`meeting_summary_context.context_prompt` and the attachments table.

Rationale: single source of truth, no race between "user edits textarea →
debounced save in flight → generation kicks off with stale value". The
backend reads the latest persisted state.

Tradeoff: if the user wants a "try this generation with different context
without saving", they can't. Acceptable — they can edit, wait 500 ms for
autosave, then generate.

### 5.5 Drag-and-drop

Use Tauri's `webview.onDragDropEvent` rather than the HTML5 DnD API. The HTML
API doesn't get file paths from native drags on Tauri webviews. The Rust side
exposes the dropped paths and we route them through the same
`api_add_context_attachment` command.

Scope of the drop zone: only when the cursor is over the context textarea
region. Other drop zones in the app (e.g., import audio) keep their own
handlers; we filter by component-level state to avoid global conflicts.

## 6. Error handling

| Scenario | Behavior |
|---|---|
| Source file missing/unreadable | Toast: "Não foi possível ler o arquivo: {path}". DB unchanged. |
| Non-text (binary) | Toast: "Apenas arquivos de texto são suportados nesta versão". |
| Meeting folder not yet created | Toast: "Comece uma gravação antes de anexar arquivos a esta reunião". |
| Disk write failure | Toast generic error; DB row not created. |
| DB write failure after disk write | Roll back: delete the stored file; toast generic error. |
| Open attachment when file is gone (deleted externally) | Toast: "Arquivo não encontrado. Remova e re-anexe."; do not delete the DB row automatically. |
| Generation with attachment file missing | Skip that attachment; log warning; continue. Do NOT fail the whole generation. |

## 7. Testing

### Rust unit tests

- `storage.rs`:
  - Reject binary file (file containing NUL bytes).
  - Truncate at last newline when over 256 KB.
  - Sanitize filename with `..`, `/`, control chars.
  - Cleanup on rollback (DB write fails after disk write).
- `prompt_builder.rs`:
  - Renders zero attachments → empty block (or omitted block).
  - Renders multiple attachments in `sort_order` then `created_at`.
  - Escapes `<`, `>`, `&`, `"` in `display_name`.
  - Emits `truncated="true"` only when flag is set.
- `repository.rs`:
  - Cascade delete: deleting meeting deletes context row + all attachments.

### Rust integration tests (with temp DB + temp folder)

- Add 3 attachments, list returns 3 in correct order.
- Remove one, list returns 2; stored file is gone.
- Save context, get returns same string.
- Save empty context, get returns "" (not NULL handling test).

### Frontend tests

- `useSummaryContext`:
  - Debounced save: typing 5 times in 200 ms results in 1 save call after
    500 ms idle.
  - Initial load fills state.
- `ContextAttachmentsBar`:
  - Renders truncated badge when flag is set.
  - Calls `onRemove(id)` on chip ✕ click.

### Manual verification

- Add file → reload page → file is still there.
- Add file → start regeneration → log shows attachment in prompt.
- Delete meeting → `<meeting_folder>/attachments/` is gone with the folder.
- Drag-and-drop a `.md` file → appears as chip.
- Drag-and-drop a `.png` (binary) → toast error, nothing added.

## 8. Migration & rollout

- One SQL migration; idempotent (`CREATE TABLE IF NOT EXISTS`).
- No data migration needed — pre-existing meetings simply have no
  `meeting_summary_context` row (the get-command returns empty defaults when
  no row exists).
- Feature is purely additive: the old behavior (one-shot customPrompt typed
  before generation) still works because the LLM still receives a
  `<user_context>` block — it just comes from persisted state now.
- No feature flag needed.

## 9. Open questions (resolved)

| Question | Decision |
|---|---|
| File types? | Text only |
| Storage? | Hybrid: copy to meeting folder + keep original path |
| UI placement? | Chips + paperclip inline with textarea |
| Token-budget warning? | Deferred (v2) |
| Provider-native attachment APIs? | No, inline as text uniformly |
| Drag-and-drop? | Yes, via Tauri native DnD |
| Upstream contribution? | After implementation lands in fork |

## 10. Out-of-scope follow-ups (v2 candidates)

- PDF support (via `pdf-extract` crate).
- Image attachments with vision-capable model gating.
- Token-budget warning per provider/model.
- Reorder attachments by drag.
- Share context across meetings (templates).
- Per-attachment preview in UI.
