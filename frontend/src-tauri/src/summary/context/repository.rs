use crate::summary::context::types::{ContextAttachmentInfo, ContextAttachmentRow};
use chrono::Utc;
use sqlx::SqlitePool;

pub struct SummaryContextRepository;

impl SummaryContextRepository {
    /// Returns the persisted context_prompt for a meeting, or empty string if
    /// no row exists.
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

    /// Returns the persisted template_id for a meeting, or None if no row exists
    /// or no template was saved.
    pub async fn get_template(
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT template_id FROM meeting_summary_context WHERE meeting_id = ?",
        )
        .bind(meeting_id)
        .fetch_optional(pool)
        .await?;
        Ok(row.and_then(|(t,)| t))
    }

    /// Upsert the template_id for a meeting without touching context_prompt
    /// (which keeps its column default on insert).
    pub async fn upsert_template(
        pool: &SqlitePool,
        meeting_id: &str,
        template_id: &str,
    ) -> Result<(), sqlx::Error> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"INSERT INTO meeting_summary_context (meeting_id, template_id, updated_at)
               VALUES (?, ?, ?)
               ON CONFLICT(meeting_id) DO UPDATE SET
                 template_id = excluded.template_id,
                 updated_at = excluded.updated_at"#,
        )
        .bind(meeting_id)
        .bind(template_id)
        .bind(&now)
        .execute(pool)
        .await?;
        Ok(())
    }
}

pub struct ContextAttachmentsRepository;

impl ContextAttachmentsRepository {
    /// List attachments for a meeting, in (sort_order, created_at) order.
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
        let (s,): (i64,) = sqlx::query_as(
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
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

    // -- SummaryContextRepository --------------------------------------------

    #[tokio::test]
    async fn get_prompt_returns_empty_when_no_row() {
        let pool = setup_pool().await;
        let s = SummaryContextRepository::get_prompt(&pool, "m1").await.unwrap();
        assert_eq!(s, "");
    }

    #[tokio::test]
    async fn upsert_then_get_returns_value() {
        let pool = setup_pool().await;
        SummaryContextRepository::upsert_prompt(&pool, "m1", "Hello world")
            .await
            .unwrap();
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
    async fn get_template_returns_none_when_no_row() {
        let pool = setup_pool().await;
        let t = SummaryContextRepository::get_template(&pool, "m1").await.unwrap();
        assert_eq!(t, None);
    }

    #[tokio::test]
    async fn upsert_then_get_template_returns_value() {
        let pool = setup_pool().await;
        SummaryContextRepository::upsert_template(&pool, "m1", "daily_standup")
            .await
            .unwrap();
        let t = SummaryContextRepository::get_template(&pool, "m1").await.unwrap();
        assert_eq!(t, Some("daily_standup".to_string()));
    }

    #[tokio::test]
    async fn template_and_prompt_are_independent() {
        let pool = setup_pool().await;
        // Save a template first (row created without an explicit prompt).
        SummaryContextRepository::upsert_template(&pool, "m1", "standard_meeting")
            .await
            .unwrap();
        // Prompt defaults to empty string, template preserved.
        assert_eq!(
            SummaryContextRepository::get_prompt(&pool, "m1").await.unwrap(),
            ""
        );
        // Now save a prompt; template must survive.
        SummaryContextRepository::upsert_prompt(&pool, "m1", "Hello")
            .await
            .unwrap();
        assert_eq!(
            SummaryContextRepository::get_prompt(&pool, "m1").await.unwrap(),
            "Hello"
        );
        assert_eq!(
            SummaryContextRepository::get_template(&pool, "m1").await.unwrap(),
            Some("standard_meeting".to_string())
        );
    }

    #[tokio::test]
    async fn cascade_delete_removes_context_row() {
        let pool = setup_pool().await;
        SummaryContextRepository::upsert_prompt(&pool, "m1", "Hello").await.unwrap();
        sqlx::query("PRAGMA foreign_keys = ON").execute(&pool).await.unwrap();
        sqlx::query("DELETE FROM meetings WHERE id = ?")
            .bind("m1")
            .execute(&pool)
            .await
            .unwrap();
        let s = SummaryContextRepository::get_prompt(&pool, "m1").await.unwrap();
        assert_eq!(s, "");
    }

    // -- ContextAttachmentsRepository ----------------------------------------

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
        assert_eq!(
            ContextAttachmentsRepository::count(&pool, "m1").await.unwrap(),
            2
        );
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
        let n = ContextAttachmentsRepository::next_sort_order(&pool, "m1")
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn next_sort_order_increments() {
        let pool = setup_pool().await;
        ContextAttachmentsRepository::insert(&pool, &sample_row("a1", "m1", 5))
            .await
            .unwrap();
        let n = ContextAttachmentsRepository::next_sort_order(&pool, "m1")
            .await
            .unwrap();
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
}
