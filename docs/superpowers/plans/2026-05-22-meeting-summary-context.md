# Meeting Summary Context Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist the per-meeting "Add context for AI summary" textarea, add text-file attachments that get inlined into the LLM user prompt, and rename `customPrompt` → `contextPrompt` across the stack.

**Architecture:** Two new SQLite tables (`meeting_summary_context`, `meeting_context_attachments`). New Rust module `summary/context/` exposes 5 Tauri commands. Attachments are copied to `<meeting_folder>/attachments/`. Backend loads context + attachments by `meeting_id` at generation time — frontend no longer passes the prompt as a parameter. Frontend gets a `useSummaryContext` hook and a `ContextAttachmentsBar` component above the existing textarea.

**Tech Stack:** Rust (Tauri 2, sqlx, tokio), TypeScript (Next.js, React), SQLite, Tauri native drag-and-drop API.

**Spec reference:** [`docs/superpowers/specs/2026-05-22-meeting-summary-context-design.md`](../specs/2026-05-22-meeting-summary-context-design.md).

---

## File Structure

### New files

| Path | Responsibility |
|---|---|
| `frontend/src-tauri/migrations/20260522000000_add_meeting_summary_context.sql` | Both new tables + index |
| `frontend/src-tauri/src/summary/context/mod.rs` | Module exports |
| `frontend/src-tauri/src/summary/context/types.rs` | DTO structs (serde) |
| `frontend/src-tauri/src/summary/context/repository.rs` | SQLx CRUD for both tables |
| `frontend/src-tauri/src/summary/context/storage.rs` | File copy, truncation, UTF-8 validation, sanitization |
| `frontend/src-tauri/src/summary/context/prompt_builder.rs` | Render attachments into `<attachments>` block |
| `frontend/src-tauri/src/summary/context/commands.rs` | 5 Tauri commands |
| `frontend/src/hooks/meeting-details/useSummaryContext.ts` | Frontend hook (load + debounced save + attachment ops) |
| `frontend/src/components/MeetingDetails/ContextAttachmentsBar.tsx` | Chips + paperclip button UI |

### Modified files

| Path | Change |
|---|---|
| `frontend/src-tauri/src/lib.rs` | Register 5 new commands in `invoke_handler` |
| `frontend/src-tauri/src/summary/mod.rs` | Declare and re-export `context` module + commands |
| `frontend/src-tauri/src/summary/commands.rs:176-249` | Remove `custom_prompt` param from `api_process_transcript` |
| `frontend/src-tauri/src/summary/service.rs:73-236` | Drop `custom_prompt` param; load context + attachments from DB |
| `frontend/src-tauri/src/summary/processor.rs:165,357-368` | Rename `custom_prompt` → `context_prompt`; accept and render attachments |
| `frontend/src/app/meeting-details/page-content.tsx:57,176,216` | Replace local `customPrompt` state with `useSummaryContext` |
| `frontend/src/hooks/meeting-details/useSummaryGeneration.ts:107-116` | Drop `customPrompt` from invoke + from function signature |
| `frontend/src/components/MeetingDetails/TranscriptPanel.tsx:11,35,103-110` | Rename props; mount `ContextAttachmentsBar`; native DnD |
| `frontend/src/components/MeetingDetails/SummaryPanel.tsx:36,38,74,113,153,181,193` | Rename `customPrompt` prop → `contextPrompt`; drop arg from `onGenerateSummary` call |
| `frontend/src/components/MeetingDetails/SummaryGeneratorButtonGroup.tsx:31,33,49,105,188,208` | Rename + drop arg from `onGenerateSummary` |

---

## Phase 1: Schema

### Task 1: SQL migration

**Files:**
- Create: `frontend/src-tauri/migrations/20260522000000_add_meeting_summary_context.sql`

- [ ] **Step 1: Create the migration file**

```sql
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
```

- [ ] **Step 2: Commit**

```bash
git add frontend/src-tauri/migrations/20260522000000_add_meeting_summary_context.sql
git commit -m "feat(db): add meeting_summary_context and meeting_context_attachments tables"
```

---

## Phase 2: Rust types and module skeleton

### Task 2: Module skeleton + DTO types

**Files:**
- Create: `frontend/src-tauri/src/summary/context/mod.rs`
- Create: `frontend/src-tauri/src/summary/context/types.rs`
- Modify: `frontend/src-tauri/src/summary/mod.rs:33-40`

- [ ] **Step 1: Create `mod.rs`**

```rust
// frontend/src-tauri/src/summary/context/mod.rs
//! Per-meeting context for AI summary generation.
//!
//! Owns the textarea content (`meeting_summary_context.context_prompt`) and
//! the text-file attachments (`meeting_context_attachments`). At summary
//! generation time, the loaded context is injected into the LLM user prompt.

pub mod commands;
pub mod prompt_builder;
pub mod repository;
pub mod storage;
pub mod types;

pub use commands::{
    __cmd__api_add_context_attachment, __cmd__api_get_summary_context,
    __cmd__api_open_context_attachment, __cmd__api_remove_context_attachment,
    __cmd__api_save_summary_context, api_add_context_attachment, api_get_summary_context,
    api_open_context_attachment, api_remove_context_attachment, api_save_summary_context,
};
```

- [ ] **Step 2: Create `types.rs`**

```rust
// frontend/src-tauri/src/summary/context/types.rs
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Attachment shape exposed to the frontend (no content, no internal paths).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextAttachmentInfo {
    pub id: String,
    pub display_name: String,
    pub size_bytes: i64,
    pub truncated: bool,
    pub created_at: String,
}

/// Combined response from `api_get_summary_context`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryContextData {
    pub context_prompt: String,
    pub attachments: Vec<ContextAttachmentInfo>,
}

/// Full attachment row as stored in `meeting_context_attachments`.
#[derive(Debug, Clone, FromRow)]
pub struct ContextAttachmentRow {
    pub id: String,
    pub meeting_id: String,
    pub original_path: String,
    pub stored_filename: String,
    pub display_name: String,
    pub size_bytes: i64,
    pub truncated: bool,
    pub sort_order: i64,
    pub created_at: String,
}

/// Materialized attachment passed to the prompt builder.
#[derive(Debug, Clone)]
pub struct AttachmentContent {
    pub display_name: String,
    pub content: String,
    pub truncated: bool,
}
```

- [ ] **Step 3: Wire module into `summary/mod.rs`**

In `frontend/src-tauri/src/summary/mod.rs`, after the existing `pub mod` declarations (line 33-40), add:

```rust
pub mod context;
```

And in the re-export block after the existing template re-exports (around line 53), add:

```rust
// Re-export summary context commands
pub use context::{
    __cmd__api_add_context_attachment, __cmd__api_get_summary_context,
    __cmd__api_open_context_attachment, __cmd__api_remove_context_attachment,
    __cmd__api_save_summary_context, api_add_context_attachment, api_get_summary_context,
    api_open_context_attachment, api_remove_context_attachment, api_save_summary_context,
};
```

- [ ] **Step 4: Create stub files so the module compiles**

Create empty stubs that will be filled in subsequent tasks. These stubs let the compiler accept the `pub mod` declarations.

`frontend/src-tauri/src/summary/context/repository.rs`:
```rust
// Filled in Task 3 + Task 4.
```

`frontend/src-tauri/src/summary/context/storage.rs`:
```rust
// Filled in Task 5.
```

`frontend/src-tauri/src/summary/context/prompt_builder.rs`:
```rust
// Filled in Task 6.
```

`frontend/src-tauri/src/summary/context/commands.rs`:
```rust
// Filled in Tasks 7-8.
```

NOTE: the empty `commands.rs` will fail the `__cmd__*` re-exports in `mod.rs`. Temporarily comment out the `pub use commands::{...}` block in `context/mod.rs` and `summary/mod.rs`. They get uncommented in Task 7.

- [ ] **Step 5: Commit**

```bash
git add frontend/src-tauri/src/summary/context/ frontend/src-tauri/src/summary/mod.rs
git commit -m "feat(summary): scaffold context module + DTO types"
```

---

## Phase 3: Repository layer (TDD)

### Task 3: Repository for `meeting_summary_context`

**Files:**
- Modify: `frontend/src-tauri/src/summary/context/repository.rs`

- [ ] **Step 1: Write failing tests**

Add at the top of `frontend/src-tauri/src/summary/context/repository.rs`:

```rust
use crate::summary::context::types::{ContextAttachmentRow, ContextAttachmentInfo};
use chrono::Utc;
use sqlx::SqlitePool;

pub struct SummaryContextRepository;

impl SummaryContextRepository {
    /// Returns the persisted context_prompt for a meeting, or empty string if no row exists.
    pub async fn get_prompt(
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<String, sqlx::Error> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT context_prompt FROM meeting_summary_context WHERE meeting_id = ?",
        )
        .bind(meeting_id)
        .fetch_optional(pool)
        .await?;
        Ok(row.map(|(s,)| s).unwrap_or_default())
    }

    /// Upsert the context_prompt for a meeting.
    pub async fn upsert_prompt(
        pool: &SqlitePool,
        meeting_id: &str,
        context_prompt: &str,
    ) -> Result<(), sqlx::Error> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"INSERT INTO meeting_summary_context (meeting_id, context_prompt, updated_at)
               VALUES (?, ?, ?)
               ON CONFLICT(meeting_id) DO UPDATE SET
                 context_prompt = excluded.context_prompt,
                 updated_at = excluded.updated_at"#,
        )
        .bind(meeting_id)
        .bind(context_prompt)
        .bind(&now)
        .execute(pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        // Insert a parent meeting row so FK is satisfied.
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES (?, ?, ?, ?)")
            .bind("m1")
            .bind("Test")
            .bind("2026-05-22T00:00:00Z")
            .bind("2026-05-22T00:00:00Z")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn get_prompt_returns_empty_when_no_row() {
        let pool = setup_pool().await;
        let s = SummaryContextRepository::get_prompt(&pool, "m1").await.unwrap();
        assert_eq!(s, "");
    }

    #[tokio::test]
    async fn upsert_then_get_returns_value() {
        let pool = setup_pool().await;
        SummaryContextRepository::upsert_prompt(&pool, "m1", "Hello world").await.unwrap();
        let s = SummaryContextRepository::get_prompt(&pool, "m1").await.unwrap();
        assert_eq!(s, "Hello world");
    }

    #[tokio::test]
    async fn upsert_overwrites_existing() {
        let pool = setup_pool().await;
        SummaryContextRepository::upsert_prompt(&pool, "m1", "First").await.unwrap();
        SummaryContextRepository::upsert_prompt(&pool, "m1", "Second").await.unwrap();
        let s = SummaryContextRepository::get_prompt(&pool, "m1").await.unwrap();
        assert_eq!(s, "Second");
    }

    #[tokio::test]
    async fn cascade_delete_removes_context_row() {
        let pool = setup_pool().await;
        SummaryContextRepository::upsert_prompt(&pool, "m1", "Hello").await.unwrap();
        // Enable FKs (required in SQLite to enforce CASCADE).
        sqlx::query("PRAGMA foreign_keys = ON").execute(&pool).await.unwrap();
        sqlx::query("DELETE FROM meetings WHERE id = ?")
            .bind("m1")
            .execute(&pool)
            .await
            .unwrap();
        let s = SummaryContextRepository::get_prompt(&pool, "m1").await.unwrap();
        assert_eq!(s, "");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --manifest-path frontend/src-tauri/Cargo.toml summary::context::repository::tests -- --nocapture`
Expected: all 4 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add frontend/src-tauri/src/summary/context/repository.rs
git commit -m "feat(summary/context): SummaryContextRepository with upsert/get + tests"
```

### Task 4: Repository for `meeting_context_attachments`

**Files:**
- Modify: `frontend/src-tauri/src/summary/context/repository.rs`

- [ ] **Step 1: Add attachment functions and tests**

Append below the `SummaryContextRepository` impl (and above the `#[cfg(test)]` block — move tests to the very bottom):

```rust
pub struct ContextAttachmentsRepository;

impl ContextAttachmentsRepository {
    /// List attachments for a meeting, in (sort_order, created_at) order.
    /// Returns metadata only — call `load_content` separately to read disk.
    pub async fn list(
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<Vec<ContextAttachmentRow>, sqlx::Error> {
        sqlx::query_as::<_, ContextAttachmentRow>(
            r#"SELECT id, meeting_id, original_path, stored_filename, display_name,
                      size_bytes, truncated, sort_order, created_at
               FROM meeting_context_attachments
               WHERE meeting_id = ?
               ORDER BY sort_order ASC, created_at ASC"#,
        )
        .bind(meeting_id)
        .fetch_all(pool)
        .await
    }

    /// Returns one attachment row or None.
    pub async fn get(
        pool: &SqlitePool,
        meeting_id: &str,
        attachment_id: &str,
    ) -> Result<Option<ContextAttachmentRow>, sqlx::Error> {
        sqlx::query_as::<_, ContextAttachmentRow>(
            r#"SELECT id, meeting_id, original_path, stored_filename, display_name,
                      size_bytes, truncated, sort_order, created_at
               FROM meeting_context_attachments
               WHERE meeting_id = ? AND id = ?"#,
        )
        .bind(meeting_id)
        .bind(attachment_id)
        .fetch_optional(pool)
        .await
    }

    /// Count attachments for a meeting (used for the 10-attachments limit).
    pub async fn count(pool: &SqlitePool, meeting_id: &str) -> Result<i64, sqlx::Error> {
        let (n,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM meeting_context_attachments WHERE meeting_id = ?")
                .bind(meeting_id)
                .fetch_one(pool)
                .await?;
        Ok(n)
    }

    /// Sum of size_bytes across attachments for a meeting (for the 1MB limit).
    pub async fn total_size(pool: &SqlitePool, meeting_id: &str) -> Result<i64, sqlx::Error> {
        let (s,): (i64,) =
            sqlx::query_as(
                "SELECT COALESCE(SUM(size_bytes), 0) FROM meeting_context_attachments WHERE meeting_id = ?",
            )
            .bind(meeting_id)
            .fetch_one(pool)
            .await?;
        Ok(s)
    }

    /// Insert a new attachment row.
    pub async fn insert(
        pool: &SqlitePool,
        row: &ContextAttachmentRow,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"INSERT INTO meeting_context_attachments
               (id, meeting_id, original_path, stored_filename, display_name,
                size_bytes, truncated, sort_order, created_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&row.id)
        .bind(&row.meeting_id)
        .bind(&row.original_path)
        .bind(&row.stored_filename)
        .bind(&row.display_name)
        .bind(row.size_bytes)
        .bind(row.truncated)
        .bind(row.sort_order)
        .bind(&row.created_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Delete one attachment by (meeting_id, id). Returns true if a row was deleted.
    pub async fn delete(
        pool: &SqlitePool,
        meeting_id: &str,
        attachment_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "DELETE FROM meeting_context_attachments WHERE meeting_id = ? AND id = ?",
        )
        .bind(meeting_id)
        .bind(attachment_id)
        .execute(pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Returns next sort_order value (max + 1, or 0 if empty).
    pub async fn next_sort_order(
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<i64, sqlx::Error> {
        let (max,): (Option<i64>,) = sqlx::query_as(
            "SELECT MAX(sort_order) FROM meeting_context_attachments WHERE meeting_id = ?",
        )
        .bind(meeting_id)
        .fetch_one(pool)
        .await?;
        Ok(max.map(|m| m + 1).unwrap_or(0))
    }
}

impl From<&ContextAttachmentRow> for ContextAttachmentInfo {
    fn from(row: &ContextAttachmentRow) -> Self {
        Self {
            id: row.id.clone(),
            display_name: row.display_name.clone(),
            size_bytes: row.size_bytes,
            truncated: row.truncated,
            created_at: row.created_at.clone(),
        }
    }
}
```

- [ ] **Step 2: Add tests for `ContextAttachmentsRepository`**

Append to the `#[cfg(test)] mod tests` block:

```rust
    fn sample_row(id: &str, meeting_id: &str, sort_order: i64) -> ContextAttachmentRow {
        ContextAttachmentRow {
            id: id.to_string(),
            meeting_id: meeting_id.to_string(),
            original_path: format!("/orig/{}.txt", id),
            stored_filename: format!("{}_file.txt", id),
            display_name: format!("{}.txt", id),
            size_bytes: 100,
            truncated: false,
            sort_order,
            created_at: "2026-05-22T00:00:00Z".to_string(),
        }
    }

    #[tokio::test]
    async fn list_empty_returns_empty_vec() {
        let pool = setup_pool().await;
        let rows = ContextAttachmentsRepository::list(&pool, "m1").await.unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn insert_then_list_returns_one_row() {
        let pool = setup_pool().await;
        ContextAttachmentsRepository::insert(&pool, &sample_row("a1", "m1", 0))
            .await
            .unwrap();
        let rows = ContextAttachmentsRepository::list(&pool, "m1").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "a1");
    }

    #[tokio::test]
    async fn list_orders_by_sort_order_then_created_at() {
        let pool = setup_pool().await;
        ContextAttachmentsRepository::insert(&pool, &sample_row("a2", "m1", 1))
            .await
            .unwrap();
        ContextAttachmentsRepository::insert(&pool, &sample_row("a1", "m1", 0))
            .await
            .unwrap();
        let rows = ContextAttachmentsRepository::list(&pool, "m1").await.unwrap();
        assert_eq!(rows[0].id, "a1");
        assert_eq!(rows[1].id, "a2");
    }

    #[tokio::test]
    async fn count_and_total_size() {
        let pool = setup_pool().await;
        let mut row = sample_row("a1", "m1", 0);
        row.size_bytes = 100;
        ContextAttachmentsRepository::insert(&pool, &row).await.unwrap();
        let mut row2 = sample_row("a2", "m1", 1);
        row2.size_bytes = 250;
        ContextAttachmentsRepository::insert(&pool, &row2).await.unwrap();
        assert_eq!(ContextAttachmentsRepository::count(&pool, "m1").await.unwrap(), 2);
        assert_eq!(
            ContextAttachmentsRepository::total_size(&pool, "m1").await.unwrap(),
            350
        );
    }

    #[tokio::test]
    async fn delete_returns_true_when_row_exists() {
        let pool = setup_pool().await;
        ContextAttachmentsRepository::insert(&pool, &sample_row("a1", "m1", 0))
            .await
            .unwrap();
        let deleted = ContextAttachmentsRepository::delete(&pool, "m1", "a1")
            .await
            .unwrap();
        assert!(deleted);
        let rows = ContextAttachmentsRepository::list(&pool, "m1").await.unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn delete_returns_false_when_no_row() {
        let pool = setup_pool().await;
        let deleted = ContextAttachmentsRepository::delete(&pool, "m1", "missing")
            .await
            .unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn next_sort_order_starts_at_zero() {
        let pool = setup_pool().await;
        let n = ContextAttachmentsRepository::next_sort_order(&pool, "m1").await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn next_sort_order_increments() {
        let pool = setup_pool().await;
        ContextAttachmentsRepository::insert(&pool, &sample_row("a1", "m1", 5))
            .await
            .unwrap();
        let n = ContextAttachmentsRepository::next_sort_order(&pool, "m1").await.unwrap();
        assert_eq!(n, 6);
    }

    #[tokio::test]
    async fn cascade_delete_removes_attachments() {
        let pool = setup_pool().await;
        ContextAttachmentsRepository::insert(&pool, &sample_row("a1", "m1", 0))
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON").execute(&pool).await.unwrap();
        sqlx::query("DELETE FROM meetings WHERE id = ?")
            .bind("m1")
            .execute(&pool)
            .await
            .unwrap();
        let rows = ContextAttachmentsRepository::list(&pool, "m1").await.unwrap();
        assert!(rows.is_empty());
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test --manifest-path frontend/src-tauri/Cargo.toml summary::context::repository::tests -- --nocapture`
Expected: all 12 tests PASS (4 from Task 3 + 8 new).

- [ ] **Step 4: Commit**

```bash
git add frontend/src-tauri/src/summary/context/repository.rs
git commit -m "feat(summary/context): ContextAttachmentsRepository with CRUD + tests"
```

---

## Phase 4: Storage layer (TDD)

### Task 5: File storage (validation, truncation, copy)

**Files:**
- Modify: `frontend/src-tauri/src/summary/context/storage.rs`

- [ ] **Step 1: Implement storage primitives + tests**

Replace the contents of `frontend/src-tauri/src/summary/context/storage.rs`:

```rust
//! Filesystem operations for context attachments.

use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Maximum bytes read from a source file (anything beyond is truncated).
pub const MAX_ATTACHMENT_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub struct ReadResult {
    pub content: String,
    pub truncated: bool,
    pub size_bytes: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("file not found or unreadable: {0}")]
    NotReadable(String),
    #[error("file is not valid UTF-8 text")]
    NotText,
    #[error("meeting folder is not initialized")]
    MeetingFolderMissing,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Replace control chars and path separators in a filename; cap at 64 chars.
pub fn sanitize_basename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    // Strip any leading dots to avoid hidden files / "..".
    let trimmed = cleaned.trim_start_matches('.');
    let mut out = trimmed.to_string();
    if out.len() > 64 {
        // Truncate by char boundary, not byte boundary.
        let end = out
            .char_indices()
            .nth(64)
            .map(|(i, _)| i)
            .unwrap_or(out.len());
        out.truncate(end);
    }
    if out.is_empty() {
        "attachment".to_string()
    } else {
        out
    }
}

/// Build the stored filename `<uuid>_<sanitized basename>`.
pub fn build_stored_filename(original_name: &str) -> String {
    let short_uuid = Uuid::new_v4().to_string().split('-').next().unwrap_or("att").to_string();
    format!("{}_{}", short_uuid, sanitize_basename(original_name))
}

/// Read up to MAX_ATTACHMENT_BYTES from `src`. Reject if not valid UTF-8 text.
/// If file exceeds limit, truncate at the last newline within the window
/// (or at the window edge if no newline exists).
pub fn read_text_capped(src: &Path) -> Result<ReadResult, StorageError> {
    let bytes = std::fs::read(src)
        .map_err(|e| StorageError::NotReadable(format!("{}: {}", src.display(), e)))?;

    // Reject if it contains NUL bytes (a reliable binary indicator).
    if bytes.iter().any(|b| *b == 0) {
        return Err(StorageError::NotText);
    }

    // Reject if it isn't valid UTF-8.
    let mut text = match std::str::from_utf8(&bytes) {
        Ok(s) => s.to_string(),
        Err(_) => return Err(StorageError::NotText),
    };

    let original_len = bytes.len();
    if original_len <= MAX_ATTACHMENT_BYTES {
        return Ok(ReadResult {
            content: text,
            truncated: false,
            size_bytes: original_len,
        });
    }

    // Truncate to MAX bytes by character boundary.
    let mut cut = MAX_ATTACHMENT_BYTES;
    while !text.is_char_boundary(cut) && cut > 0 {
        cut -= 1;
    }
    text.truncate(cut);

    // Prefer the last newline so we don't cut a line in half.
    if let Some(nl) = text.rfind('\n') {
        text.truncate(nl + 1);
    }

    Ok(ReadResult {
        content: text,
        truncated: true,
        size_bytes: original_len,
    })
}

/// Resolve `<meeting_folder>/attachments/` and create it if missing.
pub fn attachments_dir(meeting_folder: &Path) -> Result<PathBuf, StorageError> {
    if !meeting_folder.exists() {
        return Err(StorageError::MeetingFolderMissing);
    }
    let dir = meeting_folder.join("attachments");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Write `content` into `<meeting_folder>/attachments/<stored_filename>`.
pub fn write_attachment(
    meeting_folder: &Path,
    stored_filename: &str,
    content: &str,
) -> Result<PathBuf, StorageError> {
    let dir = attachments_dir(meeting_folder)?;
    let dest = dir.join(stored_filename);
    std::fs::write(&dest, content)?;
    Ok(dest)
}

/// Delete a stored attachment file. Missing file is not an error.
pub fn delete_attachment(meeting_folder: &Path, stored_filename: &str) -> Result<(), StorageError> {
    let dest = meeting_folder.join("attachments").join(stored_filename);
    if dest.exists() {
        std::fs::remove_file(&dest)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn sanitize_replaces_path_separators() {
        assert_eq!(sanitize_basename("../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_basename("a\\b.txt"), "a_b.txt");
    }

    #[test]
    fn sanitize_replaces_control_chars() {
        assert_eq!(sanitize_basename("a\nb\tc.txt"), "a_b_c.txt");
    }

    #[test]
    fn sanitize_caps_at_64_chars() {
        let long = "a".repeat(100);
        let out = sanitize_basename(&long);
        assert_eq!(out.len(), 64);
    }

    #[test]
    fn sanitize_handles_empty() {
        assert_eq!(sanitize_basename(""), "attachment");
        assert_eq!(sanitize_basename("..."), "attachment");
    }

    #[test]
    fn build_stored_filename_prefixes_uuid() {
        let name = build_stored_filename("notes.md");
        assert!(name.ends_with("_notes.md"));
        let parts: Vec<&str> = name.splitn(2, '_').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1], "notes.md");
    }

    fn write_temp(dir: &TempDir, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.path().join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content).unwrap();
        p
    }

    #[test]
    fn read_text_capped_reads_small_file() {
        let dir = TempDir::new().unwrap();
        let p = write_temp(&dir, "small.md", b"hello\n");
        let r = read_text_capped(&p).unwrap();
        assert_eq!(r.content, "hello\n");
        assert!(!r.truncated);
        assert_eq!(r.size_bytes, 6);
    }

    #[test]
    fn read_text_capped_rejects_binary() {
        let dir = TempDir::new().unwrap();
        let p = write_temp(&dir, "bin", &[0x00, 0x01, 0x02]);
        let err = read_text_capped(&p).unwrap_err();
        assert!(matches!(err, StorageError::NotText));
    }

    #[test]
    fn read_text_capped_rejects_invalid_utf8() {
        let dir = TempDir::new().unwrap();
        // 0xC0 0x28 is invalid UTF-8 (and no NUL byte).
        let p = write_temp(&dir, "bad", &[0xC0, 0x28]);
        let err = read_text_capped(&p).unwrap_err();
        assert!(matches!(err, StorageError::NotText));
    }

    #[test]
    fn read_text_capped_truncates_oversized_at_newline() {
        let dir = TempDir::new().unwrap();
        // Build a file > 256KB, padded with newlines.
        let mut big = String::new();
        for _ in 0..30000 {
            big.push_str("line of about ten chars\n"); // 24 bytes each
        }
        assert!(big.len() > MAX_ATTACHMENT_BYTES);
        let p = write_temp(&dir, "big.txt", big.as_bytes());
        let r = read_text_capped(&p).unwrap();
        assert!(r.truncated);
        assert!(r.content.ends_with('\n'));
        assert!(r.content.len() <= MAX_ATTACHMENT_BYTES);
    }

    #[test]
    fn write_attachment_creates_directory_and_file() {
        let dir = TempDir::new().unwrap();
        let dest =
            write_attachment(dir.path(), "abc_notes.md", "hello").unwrap();
        assert!(dest.exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "hello");
        assert!(dir.path().join("attachments").is_dir());
    }

    #[test]
    fn write_attachment_errors_when_meeting_folder_missing() {
        let nonexistent = PathBuf::from("/nonexistent/path/that/does/not/exist");
        let res = write_attachment(&nonexistent, "x.txt", "hi");
        assert!(matches!(res.unwrap_err(), StorageError::MeetingFolderMissing));
    }

    #[test]
    fn delete_attachment_succeeds_when_file_missing() {
        let dir = TempDir::new().unwrap();
        // Pre-create attachments dir to avoid the MeetingFolderMissing path.
        std::fs::create_dir_all(dir.path().join("attachments")).unwrap();
        // No file to delete — should not error.
        delete_attachment(dir.path(), "ghost.txt").unwrap();
    }

    #[test]
    fn delete_attachment_removes_file() {
        let dir = TempDir::new().unwrap();
        write_attachment(dir.path(), "x.txt", "hi").unwrap();
        delete_attachment(dir.path(), "x.txt").unwrap();
        assert!(!dir.path().join("attachments").join("x.txt").exists());
    }
}
```

- [ ] **Step 2: Add `tempfile` and `thiserror` to dev-dependencies if missing**

Check `frontend/src-tauri/Cargo.toml`. If `tempfile` is not in `[dev-dependencies]`, add:
```toml
[dev-dependencies]
tempfile = "3"
```
If `thiserror` is not in `[dependencies]`, add:
```toml
thiserror = "1"
```

- [ ] **Step 3: Run tests**

Run: `cargo test --manifest-path frontend/src-tauri/Cargo.toml summary::context::storage::tests -- --nocapture`
Expected: all 13 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add frontend/src-tauri/src/summary/context/storage.rs frontend/src-tauri/Cargo.toml
git commit -m "feat(summary/context): file storage with UTF-8 validation, truncation, sanitization"
```

---

## Phase 5: Prompt builder (TDD)

### Task 6: Render attachments into the prompt

**Files:**
- Modify: `frontend/src-tauri/src/summary/context/prompt_builder.rs`

- [ ] **Step 1: Implement builder + tests**

Replace `frontend/src-tauri/src/summary/context/prompt_builder.rs`:

```rust
//! Renders attachments into the `<attachments>` XML block injected into the
//! LLM user prompt.

use crate::summary::context::types::AttachmentContent;

/// Escape the five XML special chars in attribute/text content.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Returns the `\n\n<attachments>...</attachments>` block, or an empty string
/// when there are no attachments.
pub fn render_attachments_block(attachments: &[AttachmentContent]) -> String {
    if attachments.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\n<attachments>\n");
    for a in attachments {
        let truncated_attr = if a.truncated { " truncated=\"true\"" } else { "" };
        out.push_str(&format!(
            "<file name=\"{}\"{}>\n{}\n</file>\n",
            xml_escape(&a.display_name),
            truncated_attr,
            a.content,
        ));
    }
    out.push_str("</attachments>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_attachments_returns_empty_string() {
        assert_eq!(render_attachments_block(&[]), "");
    }

    #[test]
    fn renders_single_file() {
        let block = render_attachments_block(&[AttachmentContent {
            display_name: "brief.md".to_string(),
            content: "hello".to_string(),
            truncated: false,
        }]);
        assert!(block.contains("<file name=\"brief.md\">\nhello\n</file>"));
        assert!(block.starts_with("\n\n<attachments>\n"));
        assert!(block.ends_with("</attachments>"));
    }

    #[test]
    fn renders_truncated_attribute_when_flag_set() {
        let block = render_attachments_block(&[AttachmentContent {
            display_name: "big.txt".to_string(),
            content: "...".to_string(),
            truncated: true,
        }]);
        assert!(block.contains("<file name=\"big.txt\" truncated=\"true\">"));
    }

    #[test]
    fn escapes_xml_specials_in_display_name() {
        let block = render_attachments_block(&[AttachmentContent {
            display_name: "a<b>&c\"d.txt".to_string(),
            content: "x".to_string(),
            truncated: false,
        }]);
        assert!(block.contains("name=\"a&lt;b&gt;&amp;c&quot;d.txt\""));
    }

    #[test]
    fn renders_multiple_files_in_order() {
        let block = render_attachments_block(&[
            AttachmentContent {
                display_name: "first.md".to_string(),
                content: "1".to_string(),
                truncated: false,
            },
            AttachmentContent {
                display_name: "second.md".to_string(),
                content: "2".to_string(),
                truncated: false,
            },
        ]);
        let first_at = block.find("first.md").unwrap();
        let second_at = block.find("second.md").unwrap();
        assert!(first_at < second_at);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --manifest-path frontend/src-tauri/Cargo.toml summary::context::prompt_builder::tests`
Expected: all 5 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add frontend/src-tauri/src/summary/context/prompt_builder.rs
git commit -m "feat(summary/context): prompt builder for <attachments> block"
```

---

## Phase 6: Tauri commands

### Task 7: Save/Get context commands

**Files:**
- Modify: `frontend/src-tauri/src/summary/context/commands.rs`
- Modify: `frontend/src-tauri/src/summary/context/mod.rs` (un-comment re-exports)
- Modify: `frontend/src-tauri/src/summary/mod.rs` (un-comment re-exports)

- [ ] **Step 1: Implement first two commands**

Replace `frontend/src-tauri/src/summary/context/commands.rs`:

```rust
//! Tauri commands for managing the per-meeting summary context.

use crate::log_info;
use crate::summary::context::repository::{
    ContextAttachmentsRepository, SummaryContextRepository,
};
use crate::summary::context::storage;
use crate::summary::context::types::{
    ContextAttachmentInfo, ContextAttachmentRow, SummaryContextData,
};
use crate::AppState;
use chrono::Utc;
use std::path::PathBuf;
use tauri::{AppHandle, Runtime};
use uuid::Uuid;

/// Maximum number of attachments per meeting.
const MAX_ATTACHMENTS_PER_MEETING: i64 = 10;
/// Maximum total bytes across all attachments for a meeting (1 MB).
const MAX_TOTAL_BYTES: i64 = 1024 * 1024;

#[tauri::command]
pub async fn api_save_summary_context(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    context_prompt: String,
) -> Result<(), String> {
    log_info!("api_save_summary_context for meeting_id: {}", &meeting_id);
    let pool = state.db_manager.pool();
    SummaryContextRepository::upsert_prompt(pool, &meeting_id, &context_prompt)
        .await
        .map_err(|e| format!("Failed to save context: {}", e))
}

#[tauri::command]
pub async fn api_get_summary_context(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<SummaryContextData, String> {
    let pool = state.db_manager.pool();
    let context_prompt = SummaryContextRepository::get_prompt(pool, &meeting_id)
        .await
        .map_err(|e| format!("Failed to load context: {}", e))?;
    let rows = ContextAttachmentsRepository::list(pool, &meeting_id)
        .await
        .map_err(|e| format!("Failed to load attachments: {}", e))?;
    let attachments = rows.iter().map(ContextAttachmentInfo::from).collect();
    Ok(SummaryContextData {
        context_prompt,
        attachments,
    })
}
```

- [ ] **Step 2: Un-comment the re-exports**

In `frontend/src-tauri/src/summary/context/mod.rs`, replace the commented block left from Task 2 with the active re-exports listed there.

In `frontend/src-tauri/src/summary/mod.rs`, do the same. (Commands not yet defined will reappear in Task 8.)

For now, ONLY re-export the two commands that exist (`api_save_summary_context`, `api_get_summary_context`). Add the remaining three exports in Task 8.

`summary/context/mod.rs`:
```rust
pub use commands::{
    __cmd__api_get_summary_context, __cmd__api_save_summary_context,
    api_get_summary_context, api_save_summary_context,
};
```

`summary/mod.rs` (append next to the existing template re-exports):
```rust
pub use context::{
    __cmd__api_get_summary_context, __cmd__api_save_summary_context,
    api_get_summary_context, api_save_summary_context,
};
```

- [ ] **Step 3: Run a focused check**

Run: `cargo check --manifest-path frontend/src-tauri/Cargo.toml`
Expected: success.

- [ ] **Step 4: Commit**

```bash
git add frontend/src-tauri/src/summary/context/commands.rs frontend/src-tauri/src/summary/context/mod.rs frontend/src-tauri/src/summary/mod.rs
git commit -m "feat(summary/context): api_save_summary_context + api_get_summary_context"
```

### Task 8: Attachment commands (add / remove / open)

**Files:**
- Modify: `frontend/src-tauri/src/summary/context/commands.rs`

- [ ] **Step 1: Add helper for resolving the meeting folder**

Append to `frontend/src-tauri/src/summary/context/commands.rs`:

```rust
/// Resolve `<meeting_folder>` from `meetings.folder_path`. Returns the path
/// or an error suitable for surfacing to the user.
async fn resolve_meeting_folder(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
) -> Result<PathBuf, String> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("DB error: {}", e))?;
    let folder = row
        .and_then(|(p,)| p)
        .ok_or_else(|| "Meeting folder is not initialized — start a recording at least once".to_string())?;
    let p = PathBuf::from(folder);
    if !p.exists() {
        return Err(format!("Meeting folder does not exist on disk: {}", p.display()));
    }
    Ok(p)
}
```

- [ ] **Step 2: Implement `api_add_context_attachment`**

```rust
#[tauri::command]
pub async fn api_add_context_attachment<R: Runtime>(
    _app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    source_path: String,
) -> Result<ContextAttachmentInfo, String> {
    let pool = state.db_manager.pool();

    // Enforce limits.
    let count = ContextAttachmentsRepository::count(pool, &meeting_id)
        .await
        .map_err(|e| format!("DB error: {}", e))?;
    if count >= MAX_ATTACHMENTS_PER_MEETING {
        return Err(format!(
            "Limite de {} anexos atingido para esta reuni\u{00e3}o",
            MAX_ATTACHMENTS_PER_MEETING
        ));
    }

    // Read + validate the source file.
    let source = PathBuf::from(&source_path);
    let original_name = source
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "attachment".to_string());
    let read = storage::read_text_capped(&source)
        .map_err(|e| format!("{}", e))?;

    // Enforce total-size cap (use truncated bytes, since that's what we store).
    let truncated_size = read.content.len() as i64;
    let total = ContextAttachmentsRepository::total_size(pool, &meeting_id)
        .await
        .map_err(|e| format!("DB error: {}", e))?;
    if total + truncated_size > MAX_TOTAL_BYTES {
        return Err("Limite total de 1 MB de anexos atingido".to_string());
    }

    let meeting_folder = resolve_meeting_folder(pool, &meeting_id).await?;
    let stored_filename = storage::build_stored_filename(&original_name);

    // Write to disk first.
    let _dest = storage::write_attachment(&meeting_folder, &stored_filename, &read.content)
        .map_err(|e| format!("Failed to write attachment: {}", e))?;

    let row = ContextAttachmentRow {
        id: Uuid::new_v4().to_string(),
        meeting_id: meeting_id.clone(),
        original_path: source_path,
        stored_filename: stored_filename.clone(),
        display_name: original_name,
        size_bytes: truncated_size,
        truncated: read.truncated,
        sort_order: ContextAttachmentsRepository::next_sort_order(pool, &meeting_id)
            .await
            .map_err(|e| format!("DB error: {}", e))?,
        created_at: Utc::now().to_rfc3339(),
    };

    // Insert DB row. If it fails, roll back the file write.
    if let Err(e) = ContextAttachmentsRepository::insert(pool, &row).await {
        let _ = storage::delete_attachment(&meeting_folder, &stored_filename);
        return Err(format!("Failed to record attachment: {}", e));
    }

    log_info!(
        "Added attachment {} to meeting {} ({} bytes, truncated={})",
        row.display_name, meeting_id, row.size_bytes, row.truncated
    );

    Ok(ContextAttachmentInfo::from(&row))
}
```

- [ ] **Step 3: Implement `api_remove_context_attachment`**

```rust
#[tauri::command]
pub async fn api_remove_context_attachment(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    attachment_id: String,
) -> Result<(), String> {
    let pool = state.db_manager.pool();
    let row = ContextAttachmentsRepository::get(pool, &meeting_id, &attachment_id)
        .await
        .map_err(|e| format!("DB error: {}", e))?
        .ok_or_else(|| "Attachment not found".to_string())?;

    let meeting_folder = resolve_meeting_folder(pool, &meeting_id).await?;
    // Best-effort delete; do not fail if file is already gone.
    let _ = storage::delete_attachment(&meeting_folder, &row.stored_filename);

    ContextAttachmentsRepository::delete(pool, &meeting_id, &attachment_id)
        .await
        .map_err(|e| format!("DB error: {}", e))?;
    Ok(())
}
```

- [ ] **Step 4: Implement `api_open_context_attachment`**

```rust
#[tauri::command]
pub async fn api_open_context_attachment<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    attachment_id: String,
) -> Result<(), String> {
    let pool = state.db_manager.pool();
    let row = ContextAttachmentsRepository::get(pool, &meeting_id, &attachment_id)
        .await
        .map_err(|e| format!("DB error: {}", e))?
        .ok_or_else(|| "Attachment not found".to_string())?;

    let meeting_folder = resolve_meeting_folder(pool, &meeting_id).await?;
    let stored = meeting_folder.join("attachments").join(&row.stored_filename);
    if !stored.exists() {
        return Err("Arquivo n\u{00e3}o encontrado. Remova e re-anexe.".to_string());
    }
    // Use Tauri's opener plugin (already a project dependency for `open_meeting_folder`).
    let stored_str = stored.to_string_lossy().to_string();
    tauri_plugin_opener::OpenerExt::opener(&app)
        .open_path(stored_str, None::<&str>)
        .map_err(|e| format!("Failed to open attachment: {}", e))
}
```

NOTE: If `tauri_plugin_opener` isn't the actual crate path used by this project's `open_meeting_folder` command, mirror whatever import that command uses (see `frontend/src-tauri/src/api/api.rs`). Adjust the import accordingly.

- [ ] **Step 5: Expand the re-exports**

Update `frontend/src-tauri/src/summary/context/mod.rs`:
```rust
pub use commands::{
    __cmd__api_add_context_attachment, __cmd__api_get_summary_context,
    __cmd__api_open_context_attachment, __cmd__api_remove_context_attachment,
    __cmd__api_save_summary_context, api_add_context_attachment, api_get_summary_context,
    api_open_context_attachment, api_remove_context_attachment, api_save_summary_context,
};
```

Same in `frontend/src-tauri/src/summary/mod.rs`.

- [ ] **Step 6: Commit**

```bash
git add frontend/src-tauri/src/summary/context/commands.rs frontend/src-tauri/src/summary/context/mod.rs frontend/src-tauri/src/summary/mod.rs
git commit -m "feat(summary/context): add/remove/open attachment commands"
```

### Task 9: Register new commands in `lib.rs`

**Files:**
- Modify: `frontend/src-tauri/src/lib.rs:675-679`

- [ ] **Step 1: Add the 5 new commands**

Find the comment `// Summary commands` (around line 675). After the existing 4 summary commands, insert:

```rust
            // Summary context commands
            summary::api_save_summary_context,
            summary::api_get_summary_context,
            summary::api_add_context_attachment,
            summary::api_remove_context_attachment,
            summary::api_open_context_attachment,
```

- [ ] **Step 2: Commit**

```bash
git add frontend/src-tauri/src/lib.rs
git commit -m "feat(tauri): register summary context commands"
```

---

## Phase 7: Wire backend summary pipeline to the new context

### Task 10: Rename `custom_prompt` → `context_prompt` and add attachments param in `processor.rs`

**Files:**
- Modify: `frontend/src-tauri/src/summary/processor.rs:146,165,357-368`

- [ ] **Step 1: Update signature and doc comment**

In `frontend/src-tauri/src/summary/processor.rs`, find the signature of `generate_meeting_summary` (around line 165). Change:
- doc line `/// * `custom_prompt` - Optional user-provided context` → `/// * `context_prompt` - Persisted per-meeting context text (textarea)` and add new line `/// * `attachments` - File attachments to inline into the user prompt`
- parameter `custom_prompt: &str` → `context_prompt: &str,`
- add parameter after `context_prompt`: `attachments: &[crate::summary::context::types::AttachmentContent],`

- [ ] **Step 2: Update the prompt construction**

Replace the block at lines 357-368:

```rust
    final_user_prompt.push_str(&format!(
        r#"<transcript_chunks>
{}
</transcript_chunks>"#,
        content_to_summarize
    ));

    // Inline file attachments.
    final_user_prompt
        .push_str(&crate::summary::context::prompt_builder::render_attachments_block(
            attachments,
        ));

    if !context_prompt.is_empty() {
        final_user_prompt.push_str("\n\nUser Provided Context:\n\n<user_context>\n");
        final_user_prompt.push_str(context_prompt);
        final_user_prompt.push_str("\n</user_context>");
    }
```

- [ ] **Step 3: Update internal references (variable use)**

Search within `processor.rs` for any remaining `custom_prompt` identifier reference (not just the parameter). Replace each with `context_prompt`.

- [ ] **Step 4: Commit**

```bash
git add frontend/src-tauri/src/summary/processor.rs
git commit -m "refactor(summary): rename custom_prompt to context_prompt; accept attachments"
```

### Task 11: Update `service.rs` to load context + attachments and pass them through

**Files:**
- Modify: `frontend/src-tauri/src/summary/service.rs:73-236`

- [ ] **Step 1: Replace the `custom_prompt: String` parameter**

In the signature of `SummaryService::process_transcript_background` (around line 73-82), remove the `custom_prompt: String` parameter. Do NOT add a replacement — the function will load it from the DB itself.

- [ ] **Step 2: Load context_prompt and attachments before the processor call**

Inside `process_transcript_background`, just before the call to `generate_meeting_summary` (around line 230-236), add:

```rust
        // Load persisted context for this meeting.
        let context_prompt = crate::summary::context::repository::SummaryContextRepository::get_prompt(
            &pool, &meeting_id,
        )
        .await
        .unwrap_or_default();

        // Load attachments and materialize their content from disk.
        let attachments = {
            use crate::summary::context::{repository::ContextAttachmentsRepository, storage, types::AttachmentContent};
            let rows = ContextAttachmentsRepository::list(&pool, &meeting_id)
                .await
                .unwrap_or_default();
            // Resolve the meeting folder once; if missing, attachments are skipped.
            let folder_row: Option<(Option<String>,)> =
                sqlx::query_as("SELECT folder_path FROM meetings WHERE id = ?")
                    .bind(&meeting_id)
                    .fetch_optional(&pool)
                    .await
                    .unwrap_or(None);
            let folder = folder_row.and_then(|(p,)| p).map(std::path::PathBuf::from);
            let mut out = Vec::with_capacity(rows.len());
            if let Some(folder) = folder {
                for row in rows {
                    let path = folder.join("attachments").join(&row.stored_filename);
                    match std::fs::read_to_string(&path) {
                        Ok(content) => out.push(AttachmentContent {
                            display_name: row.display_name,
                            content,
                            truncated: row.truncated,
                        }),
                        Err(e) => {
                            tracing::warn!(
                                "Skipping attachment {} (read failed): {}",
                                row.stored_filename,
                                e
                            );
                        }
                    }
                    let _ = &storage::MAX_ATTACHMENT_BYTES; // suppress unused-import warning if module otherwise unused
                }
            }
            out
        };
```

- [ ] **Step 3: Update the call to `generate_meeting_summary`**

Replace the parameter passing for the processor call at line 236. Wherever the old code passed `&custom_prompt`, replace with `&context_prompt, &attachments`. Keep argument order: in Task 10, `context_prompt` came first then `attachments`.

- [ ] **Step 4: Commit**

```bash
git add frontend/src-tauri/src/summary/service.rs
git commit -m "feat(summary): load context_prompt + attachments inside service"
```

### Task 12: Drop `custom_prompt` param from `api_process_transcript`

**Files:**
- Modify: `frontend/src-tauri/src/summary/commands.rs:176-249`

- [ ] **Step 1: Remove the parameter and the unused-Option dance**

In `api_process_transcript`:
- Delete the line `custom_prompt: Option<String>,` from the parameter list.
- Delete `let final_prompt = custom_prompt.unwrap_or_else(|| "".to_string());`
- In the `SummaryService::process_transcript_background(...)` call, remove the `final_prompt` argument.

- [ ] **Step 2: Commit**

```bash
git add frontend/src-tauri/src/summary/commands.rs
git commit -m "refactor(summary): drop custom_prompt param from api_process_transcript"
```

---

## Phase 8: Frontend rename (mechanical refactor)

### Task 13: Rename `customPrompt` → `contextPrompt` (TypeScript)

**Files:**
- Modify: `frontend/src/components/MeetingDetails/TranscriptPanel.tsx`
- Modify: `frontend/src/components/MeetingDetails/SummaryPanel.tsx`
- Modify: `frontend/src/components/MeetingDetails/SummaryGeneratorButtonGroup.tsx`
- Modify: `frontend/src/app/meeting-details/page-content.tsx`
- Modify: `frontend/src/hooks/meeting-details/useSummaryGeneration.ts`

- [ ] **Step 1: Rename in TranscriptPanel.tsx**

In `TranscriptPanel.tsx`:
- Rename prop type field `customPrompt: string;` → `contextPrompt: string;`
- Rename prop type field `onPromptChange: (value: string) => void;` → `onContextPromptChange: (value: string) => void;`
- Update destructured args in the component.
- Update `value={customPrompt}` → `value={contextPrompt}` and `onChange={(e) => onPromptChange(e.target.value)}` → `onChange={(e) => onContextPromptChange(e.target.value)}`.

- [ ] **Step 2: Rename in SummaryPanel.tsx**

In `SummaryPanel.tsx`:
- Change `onGenerateSummary: (customPrompt: string) => Promise<void>;` → `onGenerateSummary: () => Promise<void>;`
- Change `customPrompt: string;` prop → remove entirely (no longer passed down).
- Change all internal references; the `onGenerate={() => onGenerateSummary(customPrompt)}` call (line 193) becomes `onGenerate={() => onGenerateSummary()}`.
- Remove the `customPrompt={customPrompt}` props passed to children (lines 113, 153, 181); they no longer need it.

- [ ] **Step 3: Rename in SummaryGeneratorButtonGroup.tsx**

In `SummaryGeneratorButtonGroup.tsx`:
- Change `onGenerateSummary: (customPrompt: string) => Promise<void>;` → `onGenerateSummary: () => Promise<void>;`
- Remove the `customPrompt: string;` prop.
- Replace all `onGenerateSummary(customPrompt)` calls (lines 105, 188, 208) with `onGenerateSummary()`.

- [ ] **Step 4: Rename in page-content.tsx**

In `page-content.tsx`:
- Line 57: leave the `useState` for now — Task 19 replaces it with `useSummaryContext`. For this task, simply rename `customPrompt`/`setCustomPrompt` to `contextPrompt`/`setContextPrompt` for consistency.
- Lines 176, 216: pass the renamed value. The `SummaryPanel` no longer accepts `customPrompt` — drop that prop. The `TranscriptPanel` becomes:
  ```tsx
  contextPrompt={contextPrompt}
  onContextPromptChange={setContextPrompt}
  ```

- [ ] **Step 5: Rename in useSummaryGeneration.ts**

In `useSummaryGeneration.ts`:
- Drop the `customPrompt = ''` argument from `processSummary` (lines 63, 67).
- Remove lines 96-98 (the `customPrompt.trim().length > 0` analytics check — Task 19 reintroduces it with the new variable).
- Remove `customPrompt: customPrompt,` from the `invoke('api_process_transcript', ...)` call (line 114).
- `handleGenerateSummary` (line 393) loses its `customPrompt` parameter; line 568 stops passing it.

- [ ] **Step 6: Verify the dev server builds**

Run: `cd frontend && pnpm run dev`
Expected: no TypeScript errors in the modified files (Next.js dev server compiles). Stop the dev server after the check.

- [ ] **Step 7: Commit**

```bash
git add frontend/src
git commit -m "refactor(ui): rename customPrompt to contextPrompt across components"
```

---

## Phase 9: Frontend hook + UI

### Task 14: `useSummaryContext` hook — basic load + manual save

**Files:**
- Create: `frontend/src/hooks/meeting-details/useSummaryContext.ts`

- [ ] **Step 1: Implement the hook**

```ts
// frontend/src/hooks/meeting-details/useSummaryContext.ts
import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';

export interface ContextAttachment {
  id: string;
  display_name: string;
  size_bytes: number;
  truncated: boolean;
  created_at: string;
}

export interface SummaryContextData {
  context_prompt: string;
  attachments: ContextAttachment[];
}

export function useSummaryContext(meetingId: string | undefined) {
  const [contextPrompt, setContextPromptState] = useState('');
  const [attachments, setAttachments] = useState<ContextAttachment[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Load initial state.
  useEffect(() => {
    if (!meetingId) {
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    invoke<SummaryContextData>('api_get_summary_context', { meetingId })
      .then((data) => {
        if (cancelled) return;
        setContextPromptState(data.context_prompt ?? '');
        setAttachments(data.attachments ?? []);
        setError(null);
      })
      .catch((e) => {
        if (cancelled) return;
        console.error('Failed to load summary context:', e);
        setError(String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [meetingId]);

  // Persist with 500ms debounce.
  const setContextPrompt = useCallback(
    (value: string) => {
      setContextPromptState(value);
      if (!meetingId) return;
      if (debounceRef.current) clearTimeout(debounceRef.current);
      debounceRef.current = setTimeout(() => {
        invoke('api_save_summary_context', {
          meetingId,
          contextPrompt: value,
        }).catch((e) => {
          console.error('Failed to save context:', e);
          toast.error('Falha ao salvar contexto', { description: String(e) });
        });
      }, 500);
    },
    [meetingId]
  );

  // Flush pending save on unmount.
  useEffect(() => {
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, []);

  const addAttachment = useCallback(
    async (sourcePath: string) => {
      if (!meetingId) return;
      try {
        const info = await invoke<ContextAttachment>('api_add_context_attachment', {
          meetingId,
          sourcePath,
        });
        setAttachments((prev) => [...prev, info]);
        if (info.truncated) {
          toast.warning(`Arquivo "${info.display_name}" foi truncado em 256 KB`);
        }
      } catch (e) {
        toast.error('Falha ao anexar arquivo', { description: String(e) });
      }
    },
    [meetingId]
  );

  const removeAttachment = useCallback(
    async (attachmentId: string) => {
      if (!meetingId) return;
      try {
        await invoke('api_remove_context_attachment', { meetingId, attachmentId });
        setAttachments((prev) => prev.filter((a) => a.id !== attachmentId));
      } catch (e) {
        toast.error('Falha ao remover anexo', { description: String(e) });
      }
    },
    [meetingId]
  );

  const openAttachment = useCallback(
    async (attachmentId: string) => {
      if (!meetingId) return;
      try {
        await invoke('api_open_context_attachment', { meetingId, attachmentId });
      } catch (e) {
        toast.error('Falha ao abrir anexo', { description: String(e) });
      }
    },
    [meetingId]
  );

  return {
    contextPrompt,
    setContextPrompt,
    attachments,
    addAttachment,
    removeAttachment,
    openAttachment,
    loading,
    error,
  };
}
```

- [ ] **Step 2: Commit**

```bash
git add frontend/src/hooks/meeting-details/useSummaryContext.ts
git commit -m "feat(ui): useSummaryContext hook with debounced autosave + attachments"
```

### Task 15: `ContextAttachmentsBar` component

**Files:**
- Create: `frontend/src/components/MeetingDetails/ContextAttachmentsBar.tsx`

- [ ] **Step 1: Implement the component**

```tsx
// frontend/src/components/MeetingDetails/ContextAttachmentsBar.tsx
"use client";

import { Paperclip, X, AlertTriangle } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import type { ContextAttachment } from '@/hooks/meeting-details/useSummaryContext';

interface ContextAttachmentsBarProps {
  attachments: ContextAttachment[];
  onAdd: (sourcePath: string) => Promise<void>;
  onRemove: (id: string) => Promise<void>;
  onOpen: (id: string) => Promise<void>;
  disabled?: boolean;
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

export function ContextAttachmentsBar({
  attachments,
  onAdd,
  onRemove,
  onOpen,
  disabled = false,
}: ContextAttachmentsBarProps) {
  const handlePickFile = async () => {
    try {
      const selected = await openDialog({
        multiple: false,
        filters: [
          {
            name: 'Arquivos de texto',
            extensions: ['txt', 'md', 'json', 'yaml', 'yml', 'csv', 'log',
                         'js', 'ts', 'tsx', 'jsx', 'py', 'rs', 'go', 'java',
                         'kt', 'sql', 'sh', 'html', 'css', 'xml', 'toml', 'ini'],
          },
        ],
      });
      if (typeof selected === 'string') {
        await onAdd(selected);
      }
    } catch (e) {
      console.error('File picker failed:', e);
    }
  };

  return (
    <div className="px-1 pt-2 pb-1 border-t border-gray-200">
      {attachments.length > 0 && (
        <div className="flex flex-wrap gap-1.5 mb-2 px-2">
          {attachments.map((a) => (
            <div
              key={a.id}
              className="inline-flex items-center gap-1.5 px-2 py-1 bg-gray-100 hover:bg-gray-200 rounded-md text-xs cursor-pointer transition-colors"
              title={`${a.display_name} — ${formatBytes(a.size_bytes)}${a.truncated ? ' (truncado)' : ''}`}
            >
              <Paperclip size={12} className="text-gray-600" />
              <span
                onClick={() => onOpen(a.id)}
                className="max-w-[160px] truncate"
              >
                {a.display_name}
              </span>
              {a.truncated && (
                <AlertTriangle size={12} className="text-amber-500" />
              )}
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  onRemove(a.id);
                }}
                className="text-gray-500 hover:text-red-600"
                aria-label="Remover anexo"
              >
                <X size={12} />
              </button>
            </div>
          ))}
        </div>
      )}
      <div className="flex justify-end px-2">
        <button
          type="button"
          onClick={handlePickFile}
          disabled={disabled || attachments.length >= 10}
          className="inline-flex items-center gap-1 px-2 py-1 text-xs text-gray-600 hover:text-gray-900 disabled:opacity-50 disabled:cursor-not-allowed"
          title={attachments.length >= 10 ? 'Limite de 10 anexos atingido' : 'Anexar arquivo de texto'}
        >
          <Paperclip size={14} />
          Anexar
        </button>
      </div>
    </div>
  );
}
```

NOTE: this uses `@tauri-apps/plugin-dialog` for the OS file picker. Verify the plugin is already installed (look in `frontend/package.json`). If not, `pnpm add @tauri-apps/plugin-dialog` and register in `frontend/src-tauri/Cargo.toml` + `tauri.conf.json` per Tauri docs.

- [ ] **Step 2: Commit**

```bash
git add frontend/src/components/MeetingDetails/ContextAttachmentsBar.tsx
git commit -m "feat(ui): ContextAttachmentsBar with chips + file picker"
```

### Task 16: Wire `ContextAttachmentsBar` and `useSummaryContext` into `TranscriptPanel`

**Files:**
- Modify: `frontend/src/components/MeetingDetails/TranscriptPanel.tsx`

- [ ] **Step 1: Replace the prop-based context with a prop bag**

Add `meetingId` (already passed as `meetingId` prop) and remove `contextPrompt` / `onContextPromptChange` props, since the panel will own the hook integration. Actually keep the panel "dumb" — accept the hook outputs as props from the parent. Update the props:

```tsx
interface TranscriptPanelProps {
  transcripts: Transcript[];
  contextPrompt: string;
  onContextPromptChange: (value: string) => void;
  attachments: ContextAttachment[];
  onAddAttachment: (sourcePath: string) => Promise<void>;
  onRemoveAttachment: (id: string) => Promise<void>;
  onOpenAttachment: (id: string) => Promise<void>;
  // ...rest of existing props unchanged
}
```

(Import `ContextAttachment` from the hook file.)

- [ ] **Step 2: Render the bar above the textarea**

Replace the existing `{!isRecording && convertedSegments.length > 0 && (...)}` block with:

```tsx
{!isRecording && convertedSegments.length > 0 && (
  <div>
    <ContextAttachmentsBar
      attachments={attachments}
      onAdd={onAddAttachment}
      onRemove={onRemoveAttachment}
      onOpen={onOpenAttachment}
    />
    <div className="p-1 border-t border-gray-200">
      <textarea
        placeholder="Add context for AI summary. For example people involved, meeting overview, objective etc..."
        className="w-full px-3 py-2 border border-gray-200 rounded-md text-sm focus:outline-none focus:ring-1 focus:ring-blue-500 focus:border-blue-500 bg-white shadow-sm min-h-[80px] resize-y"
        value={contextPrompt}
        onChange={(e) => onContextPromptChange(e.target.value)}
      />
    </div>
  </div>
)}
```

Add the import at the top:
```tsx
import { ContextAttachmentsBar } from './ContextAttachmentsBar';
import type { ContextAttachment } from '@/hooks/meeting-details/useSummaryContext';
```

- [ ] **Step 3: Commit**

```bash
git add frontend/src/components/MeetingDetails/TranscriptPanel.tsx
git commit -m "feat(ui): mount ContextAttachmentsBar above context textarea"
```

### Task 17: Replace local state with `useSummaryContext` in `page-content`

**Files:**
- Modify: `frontend/src/app/meeting-details/page-content.tsx`

- [ ] **Step 1: Swap state for hook**

Remove (around line 57):
```tsx
const [contextPrompt, setContextPrompt] = useState<string>('');
```

Add:
```tsx
const summaryContext = useSummaryContext(meeting.id);
```

Add the import:
```tsx
import { useSummaryContext } from '@/hooks/meeting-details/useSummaryContext';
```

- [ ] **Step 2: Pipe hook into `TranscriptPanel`**

Replace the prop block sent to `TranscriptPanel` (around line 174-194):

```tsx
<TranscriptPanel
  transcripts={meetingData.transcripts}
  contextPrompt={summaryContext.contextPrompt}
  onContextPromptChange={summaryContext.setContextPrompt}
  attachments={summaryContext.attachments}
  onAddAttachment={summaryContext.addAttachment}
  onRemoveAttachment={summaryContext.removeAttachment}
  onOpenAttachment={summaryContext.openAttachment}
  onCopyTranscript={copyOperations.handleCopyTranscript}
  onOpenMeetingFolder={meetingOperations.handleOpenMeetingFolder}
  isRecording={isRecording}
  disableAutoScroll={true}
  usePagination={true}
  segments={segments}
  hasMore={hasMore}
  isLoadingMore={isLoadingMore}
  totalCount={totalCount}
  loadedCount={loadedCount}
  onLoadMore={onLoadMore}
  meetingId={meeting.id}
  meetingFolderPath={meeting.folder_path}
  onRefetchTranscripts={onRefetchTranscripts}
/>
```

- [ ] **Step 3: Update `SummaryPanel` props**

Remove the `customPrompt={contextPrompt}` line (now stale).

- [ ] **Step 4: Commit**

```bash
git add frontend/src/app/meeting-details/page-content.tsx
git commit -m "feat(ui): wire useSummaryContext into meeting details page"
```

---

## Phase 10: Native drag-and-drop

### Task 18: Tauri webview drag-drop integration

**Files:**
- Modify: `frontend/src/components/MeetingDetails/TranscriptPanel.tsx`

- [ ] **Step 1: Add a drop-zone effect listening to Tauri DnD events**

In `TranscriptPanel.tsx`, import:
```tsx
import { useEffect, useRef, useState } from 'react';
import { getCurrentWebview } from '@tauri-apps/api/webview';
```

Inside the component, add a ref for the bottom region and DnD wiring:

```tsx
const dropRef = useRef<HTMLDivElement>(null);
const [dragOver, setDragOver] = useState(false);

useEffect(() => {
  const webview = getCurrentWebview();
  let unlisten: (() => void) | null = null;

  (async () => {
    unlisten = await webview.onDragDropEvent((event) => {
      const el = dropRef.current;
      if (!el) return;
      const rect = el.getBoundingClientRect();
      const inside =
        event.payload.position &&
        event.payload.position.x >= rect.left &&
        event.payload.position.x <= rect.right &&
        event.payload.position.y >= rect.top &&
        event.payload.position.y <= rect.bottom;

      if (event.payload.type === 'over') {
        setDragOver(Boolean(inside));
      } else if (event.payload.type === 'drop') {
        setDragOver(false);
        if (inside && event.payload.paths) {
          for (const path of event.payload.paths) {
            void onAddAttachment(path);
          }
        }
      } else if (event.payload.type === 'leave') {
        setDragOver(false);
      }
    });
  })();

  return () => {
    if (unlisten) unlisten();
  };
}, [onAddAttachment]);
```

Wrap the textarea + bar block with the ref-bearing div, and apply a subtle highlight on `dragOver`:

```tsx
<div
  ref={dropRef}
  className={dragOver ? 'bg-blue-50 ring-1 ring-blue-300' : undefined}
>
  {/* existing ContextAttachmentsBar + textarea */}
</div>
```

- [ ] **Step 2: Verify in dev**

Run: `cd frontend && pnpm run tauri:dev` (or platform equivalent). Drop a `.md` into the context region — chip appears. Drop a `.png` — toast error.

- [ ] **Step 3: Commit**

```bash
git add frontend/src/components/MeetingDetails/TranscriptPanel.tsx
git commit -m "feat(ui): native drag-drop for context attachments"
```

---

## Phase 11: Restore the analytics call (with the new variable)

### Task 19: Restore `trackCustomPromptUsed` analytics

**Files:**
- Modify: `frontend/src/hooks/meeting-details/useSummaryGeneration.ts`

- [ ] **Step 1: Load context_prompt at generation time and re-emit the analytics event**

`useSummaryGeneration.processSummary` no longer receives `customPrompt`. To keep the event firing (per spec §5.6 — analytics names unchanged), fetch the current `context_prompt` from the DB right before generation and emit the analytics event when non-empty.

Inside `processSummary`, before the `invoke('api_process_transcript', ...)` call:

```ts
try {
  const ctx = await invoke<{ context_prompt: string; attachments: unknown[] }>(
    'api_get_summary_context',
    { meetingId: meeting.id }
  );
  if (ctx.context_prompt.trim().length > 0) {
    await Analytics.trackCustomPromptUsed(ctx.context_prompt.trim().length);
  }
} catch (e) {
  // Non-fatal: analytics shouldn't block generation.
  console.warn('Could not fetch context for analytics:', e);
}
```

- [ ] **Step 2: Commit**

```bash
git add frontend/src/hooks/meeting-details/useSummaryGeneration.ts
git commit -m "chore(analytics): keep custom_prompt_used event (driven by persisted context)"
```

---

## Phase 12: Manual verification

### Task 20: End-to-end QA checklist

- [ ] **Step 1: Run the app**

```bash
cd frontend && pnpm run tauri:dev
```

- [ ] **Step 2: Verify each acceptance scenario**

For each, note the result. If any fails, file a bug and STOP rather than masking with quick patches.

1. **Persistence after navigation:** open a meeting, type "Hello" in the context textarea, navigate to another meeting, navigate back. Expected: "Hello" is still there.
2. **Persistence after restart:** type a message, fully close the app, reopen, navigate to the same meeting. Expected: still there.
3. **Attach via picker:** click 📎, select a `.md` file. Expected: chip appears immediately; meeting folder gains `attachments/<uuid>_*.md`.
4. **Drag-drop a text file:** drop a `.txt` over the bottom region. Expected: chip appears; subtle blue highlight during the drag.
5. **Drag-drop a binary file:** drop a `.png`. Expected: toast error "Apenas arquivos de texto são suportados", no chip.
6. **Truncation:** attach a file >300 KB of text. Expected: chip appears with the amber ⚠ icon; tooltip mentions truncation.
7. **Remove attachment:** click ✕ on a chip. Expected: chip vanishes; stored file is gone from `attachments/`.
8. **Open attachment:** click the chip name. Expected: file opens in the OS default app.
9. **Limits:** attach 10 files. Expected: paperclip button becomes disabled with the tooltip "Limite de 10 anexos atingido".
10. **Total size limit:** attach files until total exceeds 1 MB. Expected: toast "Limite total de 1 MB de anexos atingido".
11. **Generation uses context:** type "use formal tone", attach a `glossary.md`, generate summary. Verify in the Rust log that the user prompt contains both `<user_context>` and `<attachments>` blocks. The generated summary should reflect the formal-tone request and the glossary.
12. **Re-generation reuses context:** trigger regenerate. Expected: same context + attachments are inlined again, no need to retype.
13. **Cascade delete:** delete the meeting. Expected: `<meeting_folder>/attachments/` is removed alongside the meeting folder.
14. **Empty state:** open a meeting that never had context set. Expected: textarea is empty, no chips; everything works.
15. **Template context preserved:** select a template that has a `context` field (per commit `b6bc8f7`). Expected: that block still shows up in the **system** prompt (check Rust logs); does not mix with the user textarea content.

- [ ] **Step 3: Document any deviations**

If any scenario fails, capture: which scenario, expected vs actual, relevant Rust logs (`RUST_LOG=app_lib::summary=debug`).

- [ ] **Step 4: Final commit if doc/asset changes**

If the QA pass produced no code changes, no commit is needed.

---

## Out-of-scope follow-ups

Tracked in spec §10. Not implemented here.

- PDF support
- Image attachments + vision-model gating
- Per-provider token-budget warning
- Drag-to-reorder attachments
- Cross-meeting context templates
