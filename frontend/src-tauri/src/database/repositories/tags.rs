use crate::database::models::{MeetingModel, MeetingTagLink, TagModel};
use crate::database::repositories::meeting::STATUS_COMPLETED;
use chrono::Utc;
use sqlx::{Error as SqlxError, SqlitePool};
use uuid::Uuid;

pub struct TagsRepository;

/// Trims an optional text field and turns blank values into `None`.
fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn normalize_name(name: &str) -> Result<&str, SqlxError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(SqlxError::Protocol("tag name cannot be empty".to_string()));
    }
    Ok(trimmed)
}

impl TagsRepository {
    pub async fn list_tags(pool: &SqlitePool) -> Result<Vec<TagModel>, SqlxError> {
        sqlx::query_as::<_, TagModel>(
            r#"
            SELECT
                tags.id,
                tags.name,
                tags.color,
                tags.description,
                tags.created_at,
                tags.updated_at,
                COUNT(meeting_tags.meeting_id) AS meeting_count
            FROM tags
            LEFT JOIN meeting_tags ON meeting_tags.tag_id = tags.id
            GROUP BY tags.id
            ORDER BY LOWER(tags.name) ASC
            "#,
        )
        .fetch_all(pool)
        .await
    }

    async fn get_tag(pool: &SqlitePool, tag_id: &str) -> Result<Option<TagModel>, SqlxError> {
        sqlx::query_as::<_, TagModel>(
            r#"
            SELECT
                tags.id,
                tags.name,
                tags.color,
                tags.description,
                tags.created_at,
                tags.updated_at,
                COUNT(meeting_tags.meeting_id) AS meeting_count
            FROM tags
            LEFT JOIN meeting_tags ON meeting_tags.tag_id = tags.id
            WHERE tags.id = ?
            GROUP BY tags.id
            "#,
        )
        .bind(tag_id)
        .fetch_optional(pool)
        .await
    }

    /// Creates a tag, or returns the existing one when the name is already taken
    /// (case-insensitive). An existing tag keeps its own color and description.
    pub async fn create_tag(
        pool: &SqlitePool,
        name: &str,
        color: Option<&str>,
        description: Option<&str>,
    ) -> Result<TagModel, SqlxError> {
        let trimmed = normalize_name(name)?;

        let existing: Option<(String,)> =
            sqlx::query_as("SELECT id FROM tags WHERE LOWER(name) = LOWER(?)")
                .bind(trimmed)
                .fetch_optional(pool)
                .await?;

        if let Some((id,)) = existing {
            return Self::get_tag(pool, &id)
                .await?
                .ok_or(SqlxError::RowNotFound);
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let color = normalize_optional(color);
        let description = normalize_optional(description);

        sqlx::query(
            "INSERT INTO tags (id, name, color, description, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(trimmed)
        .bind(&color)
        .bind(&description)
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await?;

        Ok(TagModel {
            id,
            name: trimmed.to_string(),
            color,
            description,
            created_at: now.clone(),
            updated_at: now,
            meeting_count: 0,
        })
    }

    /// Renames and restyles a tag. Fails when another tag already uses the name.
    pub async fn update_tag(
        pool: &SqlitePool,
        tag_id: &str,
        name: &str,
        color: Option<&str>,
        description: Option<&str>,
    ) -> Result<TagModel, SqlxError> {
        let trimmed = normalize_name(name)?;

        let conflict: Option<(String,)> =
            sqlx::query_as("SELECT id FROM tags WHERE LOWER(name) = LOWER(?) AND id != ?")
                .bind(trimmed)
                .bind(tag_id)
                .fetch_optional(pool)
                .await?;

        if conflict.is_some() {
            return Err(SqlxError::Protocol(format!(
                "a tag named \"{}\" already exists",
                trimmed
            )));
        }

        let result = sqlx::query(
            "UPDATE tags SET name = ?, color = ?, description = ?, updated_at = ? WHERE id = ?",
        )
        .bind(trimmed)
        .bind(normalize_optional(color))
        .bind(normalize_optional(description))
        .bind(Utc::now().to_rfc3339())
        .bind(tag_id)
        .execute(pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(SqlxError::RowNotFound);
        }

        Self::get_tag(pool, tag_id)
            .await?
            .ok_or(SqlxError::RowNotFound)
    }

    /// Deletes a tag and its meeting links. Returns false when the tag did not exist.
    pub async fn delete_tag(pool: &SqlitePool, tag_id: &str) -> Result<bool, SqlxError> {
        let mut tx = pool.begin().await?;

        sqlx::query("DELETE FROM meeting_tags WHERE tag_id = ?")
            .bind(tag_id)
            .execute(&mut *tx)
            .await?;

        let result = sqlx::query("DELETE FROM tags WHERE id = ?")
            .bind(tag_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn get_meeting_tags(
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<Vec<TagModel>, SqlxError> {
        if meeting_id.trim().is_empty() {
            return Err(SqlxError::Protocol(
                "meeting_id cannot be empty".to_string(),
            ));
        }

        sqlx::query_as::<_, TagModel>(
            r#"
            SELECT
                tags.id,
                tags.name,
                tags.color,
                tags.description,
                tags.created_at,
                tags.updated_at,
                (
                    SELECT COUNT(*)
                    FROM meeting_tags counts
                    WHERE counts.tag_id = tags.id
                ) AS meeting_count
            FROM tags
            INNER JOIN meeting_tags ON meeting_tags.tag_id = tags.id
            WHERE meeting_tags.meeting_id = ?
            ORDER BY LOWER(tags.name) ASC
            "#,
        )
        .bind(meeting_id)
        .fetch_all(pool)
        .await
    }

    pub async fn set_meeting_tags(
        pool: &SqlitePool,
        meeting_id: &str,
        tag_ids: Vec<String>,
    ) -> Result<Vec<TagModel>, SqlxError> {
        if meeting_id.trim().is_empty() {
            return Err(SqlxError::Protocol(
                "meeting_id cannot be empty".to_string(),
            ));
        }

        let mut tx = pool.begin().await?;
        let meeting_exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(&mut *tx)
            .await?;

        if meeting_exists.is_none() {
            tx.rollback().await?;
            return Err(SqlxError::RowNotFound);
        }

        sqlx::query("DELETE FROM meeting_tags WHERE meeting_id = ?")
            .bind(meeting_id)
            .execute(&mut *tx)
            .await?;

        let now = Utc::now().to_rfc3339();
        for tag_id in tag_ids {
            let trimmed = tag_id.trim();
            if trimmed.is_empty() {
                continue;
            }

            sqlx::query(
                r#"
                INSERT OR IGNORE INTO meeting_tags (meeting_id, tag_id, created_at)
                SELECT ?, id, ? FROM tags WHERE id = ?
                "#,
            )
            .bind(meeting_id)
            .bind(&now)
            .bind(trimmed)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Self::get_meeting_tags(pool, meeting_id).await
    }

    pub async fn get_meetings_for_tag(
        pool: &SqlitePool,
        tag_id: &str,
    ) -> Result<Vec<MeetingModel>, SqlxError> {
        if tag_id.trim().is_empty() {
            return Err(SqlxError::Protocol("tag_id cannot be empty".to_string()));
        }

        sqlx::query_as::<_, MeetingModel>(
            r#"
            SELECT meetings.*
            FROM meetings
            INNER JOIN meeting_tags ON meeting_tags.meeting_id = meetings.id
            WHERE meeting_tags.tag_id = ? AND meetings.status = ?
            ORDER BY meetings.created_at DESC
            "#,
        )
        .bind(tag_id)
        .bind(STATUS_COMPLETED)
        .fetch_all(pool)
        .await
    }

    /// Every meeting/tag pair, tags ordered by name within a meeting.
    pub async fn list_meeting_tag_links(
        pool: &SqlitePool,
    ) -> Result<Vec<MeetingTagLink>, SqlxError> {
        sqlx::query_as::<_, MeetingTagLink>(
            r#"
            SELECT meeting_tags.meeting_id, tags.id AS tag_id, tags.name, tags.color
            FROM meeting_tags
            INNER JOIN tags ON tags.id = meeting_tags.tag_id
            ORDER BY meeting_tags.meeting_id, LOWER(tags.name) ASC
            "#,
        )
        .fetch_all(pool)
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::TagsRepository;
    use chrono::Utc;
    use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("connect in-memory database");

        sqlx::query(
            r#"
            CREATE TABLE meetings (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                folder_path TEXT,
                status TEXT NOT NULL DEFAULT 'completed'
            );
            CREATE TABLE tags (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                color TEXT,
                description TEXT
            );
            CREATE TABLE meeting_tags (
                meeting_id TEXT NOT NULL,
                tag_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (meeting_id, tag_id),
                FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE,
                FOREIGN KEY (tag_id) REFERENCES tags(id) ON DELETE CASCADE
            );
            "#,
        )
        .execute(&pool)
        .await
        .expect("create test schema");

        let now = Utc::now().to_rfc3339();
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES (?, ?, ?, ?)")
            .bind("meeting-a")
            .bind("Payzli Payment Integration")
            .bind(&now)
            .bind(&now)
            .execute(&pool)
            .await
            .expect("insert meeting a");
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES (?, ?, ?, ?)")
            .bind("meeting-b")
            .bind("Soul Bank Planning")
            .bind(&now)
            .bind(&now)
            .execute(&pool)
            .await
            .expect("insert meeting b");

        pool
    }

    #[tokio::test]
    async fn create_tag_trims_name_and_lists_with_zero_count() {
        let pool = test_pool().await;

        let tag = TagsRepository::create_tag(&pool, "  Payzli  ", Some("blue"), Some("  "))
            .await
            .expect("create tag");
        assert_eq!(tag.name, "Payzli");
        assert_eq!(tag.color.as_deref(), Some("blue"));
        assert_eq!(tag.description, None);
        assert_eq!(tag.meeting_count, 0);

        let tags = TagsRepository::list_tags(&pool).await.expect("list tags");
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "Payzli");
        assert_eq!(tags[0].color.as_deref(), Some("blue"));
        assert_eq!(tags[0].meeting_count, 0);
    }

    #[tokio::test]
    async fn create_tag_with_existing_name_returns_existing_tag() {
        let pool = test_pool().await;
        let first = TagsRepository::create_tag(&pool, "Payzli", Some("blue"), None)
            .await
            .expect("create tag");

        let second = TagsRepository::create_tag(&pool, "payzli", Some("red"), None)
            .await
            .expect("create duplicate tag");

        assert_eq!(second.id, first.id);
        assert_eq!(second.color.as_deref(), Some("blue"));
        assert_eq!(TagsRepository::list_tags(&pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn assigning_tags_updates_meeting_tags_and_counts() {
        let pool = test_pool().await;
        let payzli = TagsRepository::create_tag(&pool, "Payzli", None, None)
            .await
            .expect("create payzli tag");
        let follow_up = TagsRepository::create_tag(&pool, "Follow-up", None, None)
            .await
            .expect("create follow up tag");

        TagsRepository::set_meeting_tags(
            &pool,
            "meeting-a",
            vec![payzli.id.clone(), follow_up.id.clone()],
        )
        .await
        .expect("set meeting tags");

        let meeting_tags = TagsRepository::get_meeting_tags(&pool, "meeting-a")
            .await
            .expect("get meeting tags");
        assert_eq!(
            meeting_tags
                .iter()
                .map(|tag| tag.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Follow-up", "Payzli"]
        );

        let tags = TagsRepository::list_tags(&pool).await.expect("list tags");
        assert_eq!(
            tags.iter()
                .map(|tag| (tag.name.as_str(), tag.meeting_count))
                .collect::<Vec<_>>(),
            vec![("Follow-up", 1), ("Payzli", 1)]
        );
    }

    #[tokio::test]
    async fn filtering_meetings_by_tag_returns_only_assigned_meetings() {
        let pool = test_pool().await;
        let payzli = TagsRepository::create_tag(&pool, "Payzli", None, None)
            .await
            .expect("create payzli tag");
        TagsRepository::set_meeting_tags(&pool, "meeting-a", vec![payzli.id.clone()])
            .await
            .expect("assign tag");

        let meetings = TagsRepository::get_meetings_for_tag(&pool, &payzli.id)
            .await
            .expect("filter meetings");

        assert_eq!(meetings.len(), 1);
        assert_eq!(meetings[0].id, "meeting-a");
        assert_eq!(meetings[0].title, "Payzli Payment Integration");
    }

    #[tokio::test]
    async fn filtering_meetings_by_tag_skips_meetings_still_recording() {
        let pool = test_pool().await;
        let payzli = TagsRepository::create_tag(&pool, "Payzli", None, None)
            .await
            .expect("create payzli tag");
        TagsRepository::set_meeting_tags(&pool, "meeting-a", vec![payzli.id.clone()])
            .await
            .expect("assign tag");
        sqlx::query("UPDATE meetings SET status = 'recording' WHERE id = 'meeting-a'")
            .execute(&pool)
            .await
            .expect("mark meeting as recording");

        let meetings = TagsRepository::get_meetings_for_tag(&pool, &payzli.id)
            .await
            .expect("filter meetings");

        assert!(meetings.is_empty());
    }

    #[tokio::test]
    async fn update_tag_changes_name_color_and_description() {
        let pool = test_pool().await;
        let tag = TagsRepository::create_tag(&pool, "Payzli", None, None)
            .await
            .expect("create tag");

        let updated = TagsRepository::update_tag(
            &pool,
            &tag.id,
            " Payzli Client ",
            Some("green"),
            Some("Payment integration meetings"),
        )
        .await
        .expect("update tag");

        assert_eq!(updated.id, tag.id);
        assert_eq!(updated.name, "Payzli Client");
        assert_eq!(updated.color.as_deref(), Some("green"));
        assert_eq!(
            updated.description.as_deref(),
            Some("Payment integration meetings")
        );
    }

    #[tokio::test]
    async fn update_tag_keeps_own_name_with_different_case() {
        let pool = test_pool().await;
        let tag = TagsRepository::create_tag(&pool, "Payzli", None, None)
            .await
            .expect("create tag");

        let updated = TagsRepository::update_tag(&pool, &tag.id, "PAYZLI", None, None)
            .await
            .expect("rename to same name with different case");

        assert_eq!(updated.name, "PAYZLI");
    }

    #[tokio::test]
    async fn update_tag_rejects_name_of_another_tag() {
        let pool = test_pool().await;
        TagsRepository::create_tag(&pool, "Payzli", None, None)
            .await
            .expect("create payzli tag");
        let other = TagsRepository::create_tag(&pool, "Follow-up", None, None)
            .await
            .expect("create follow up tag");

        let result = TagsRepository::update_tag(&pool, &other.id, "payzli", None, None).await;

        assert!(matches!(result, Err(sqlx::Error::Protocol(_))));
    }

    #[tokio::test]
    async fn update_missing_tag_is_not_found() {
        let pool = test_pool().await;

        let result = TagsRepository::update_tag(&pool, "missing", "Payzli", None, None).await;

        assert!(matches!(result, Err(sqlx::Error::RowNotFound)));
    }

    #[tokio::test]
    async fn delete_tag_removes_links() {
        let pool = test_pool().await;
        let payzli = TagsRepository::create_tag(&pool, "Payzli", None, None)
            .await
            .expect("create payzli tag");
        TagsRepository::set_meeting_tags(&pool, "meeting-a", vec![payzli.id.clone()])
            .await
            .expect("assign tag");

        assert!(TagsRepository::delete_tag(&pool, &payzli.id)
            .await
            .expect("delete tag"));

        assert!(TagsRepository::list_tags(&pool).await.unwrap().is_empty());
        assert!(TagsRepository::list_meeting_tag_links(&pool)
            .await
            .unwrap()
            .is_empty());
        assert!(!TagsRepository::delete_tag(&pool, &payzli.id)
            .await
            .expect("delete missing tag"));
    }

    #[tokio::test]
    async fn meeting_tag_links_cover_every_assignment() {
        let pool = test_pool().await;
        let payzli = TagsRepository::create_tag(&pool, "Payzli", Some("blue"), None)
            .await
            .expect("create payzli tag");
        let follow_up = TagsRepository::create_tag(&pool, "Follow-up", None, None)
            .await
            .expect("create follow up tag");
        TagsRepository::set_meeting_tags(
            &pool,
            "meeting-a",
            vec![payzli.id.clone(), follow_up.id.clone()],
        )
        .await
        .expect("assign meeting a");
        TagsRepository::set_meeting_tags(&pool, "meeting-b", vec![payzli.id.clone()])
            .await
            .expect("assign meeting b");

        let links = TagsRepository::list_meeting_tag_links(&pool)
            .await
            .expect("list links");

        assert_eq!(
            links
                .iter()
                .map(|l| (l.meeting_id.as_str(), l.name.as_str(), l.color.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("meeting-a", "Follow-up", None),
                ("meeting-a", "Payzli", Some("blue")),
                ("meeting-b", "Payzli", Some("blue")),
            ]
        );
    }
}
