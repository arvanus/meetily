//! Picks meeting tags with the summary LLM.
//!
//! Runs after a summary is generated: the LLM gets the tags that already exist (name and
//! optional description) plus the final summary, and answers with the names that apply.
//! It never creates tags. The answer replaces the meeting's tags.

use crate::database::models::TagModel;
use crate::database::repositories::tags::TagsRepository;
use crate::summary::llm_client::{generate_summary, LLMProvider};
use crate::summary::processor::clean_llm_markdown_output;
use reqwest::Client;
use serde::Deserialize;
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

/// Everything needed to reach the LLM already configured for the summary.
pub struct LlmTarget<'a> {
    pub client: &'a Client,
    pub provider: &'a LLMProvider,
    pub model_name: &'a str,
    pub api_key: &'a str,
    pub ollama_endpoint: Option<&'a str>,
    pub custom_openai_endpoint: Option<&'a str>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub app_data_dir: Option<&'a PathBuf>,
    pub cancellation_token: Option<&'a CancellationToken>,
}

/// Collapses newlines and repeated spaces so each tag stays on one prompt line.
fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn build_prompts(tags: &[TagModel], title: &str, summary: &str) -> (String, String) {
    let tag_lines = tags
        .iter()
        .map(|tag| match tag.description.as_deref().map(single_line) {
            Some(description) if !description.is_empty() => {
                format!("- {}: {}", single_line(&tag.name), description)
            }
            _ => format!("- {}", single_line(&tag.name)),
        })
        .collect::<Vec<_>>()
        .join("\n");

    let system_prompt = format!(
        r#"You label meeting summaries with tags.

Pick every tag from the list below that clearly applies to the meeting. Use only tags from this list and copy each name exactly as written. Do not invent tags. If no tag applies, return an empty list.

Available tags:
{}

Respond with only a JSON object and nothing else, in this exact shape:
{{"tags": ["Tag name", "Another tag"]}}"#,
        tag_lines
    );

    let user_prompt = format!(
        "Meeting title: {}\n\n<summary>\n{}\n</summary>",
        single_line(title),
        summary.trim()
    );

    (system_prompt, user_prompt)
}

#[derive(Deserialize)]
struct TagsReply {
    tags: Vec<String>,
}

/// Slice from the first `open` to the last `close`, when both exist in that order.
fn outer_span(text: &str, open: char, close: char) -> Option<&str> {
    let start = text.find(open)?;
    let end = text.rfind(close)?;
    (start < end).then(|| &text[start..=end])
}

/// Reads the tag names out of the LLM reply. Accepts `{"tags": [...]}` surrounded by
/// thinking blocks, code fences or prose, and a bare JSON array when the reply has no
/// object at all. Returns `None` when nothing parses.
pub fn parse_tag_names(response: &str) -> Option<Vec<String>> {
    let cleaned = clean_llm_markdown_output(response);

    // An object under any other shape (e.g. {"not_applicable": [...]}) must not be
    // mistaken for a tag list, so only a reply without one falls back to a bare array
    if let Some(object) = outer_span(&cleaned, '{', '}') {
        return serde_json::from_str::<TagsReply>(object).ok().map(|reply| reply.tags);
    }

    outer_span(&cleaned, '[', ']').and_then(|array| serde_json::from_str::<Vec<String>>(array).ok())
}

/// Maps names to tag ids: trimmed, case-insensitive, first occurrence wins, unknown names dropped.
pub fn resolve_tag_ids(names: &[String], tags: &[TagModel]) -> Vec<String> {
    let mut seen = HashSet::new();
    names
        .iter()
        .filter_map(|name| {
            let wanted = name.trim().to_lowercase();
            tags.iter()
                .find(|tag| tag.name.trim().to_lowercase() == wanted)
                .map(|tag| tag.id.clone())
        })
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

/// Asks the LLM which existing tags apply and replaces the meeting's tags with them.
///
/// `Ok(None)` means there was nothing to do (no tags exist or the summary is empty).
/// On any error the meeting's tags are left untouched.
pub async fn auto_tag_meeting(
    pool: &SqlitePool,
    target: &LlmTarget<'_>,
    meeting_id: &str,
    title: &str,
    summary: &str,
) -> Result<Option<Vec<TagModel>>, String> {
    if summary.trim().is_empty() {
        return Ok(None);
    }

    let tags = TagsRepository::list_tags(pool)
        .await
        .map_err(|e| format!("failed to list tags: {}", e))?;
    if tags.is_empty() {
        return Ok(None);
    }

    let (system_prompt, user_prompt) = build_prompts(&tags, title, summary);
    let response = generate_summary(
        target.client,
        target.provider,
        target.model_name,
        target.api_key,
        &system_prompt,
        &user_prompt,
        target.ollama_endpoint,
        target.custom_openai_endpoint,
        target.max_tokens,
        target.temperature,
        target.top_p,
        target.app_data_dir,
        target.cancellation_token,
    )
    .await?;

    let names = parse_tag_names(&response)
        .ok_or_else(|| format!("could not read tags from the LLM reply: {}", response.trim()))?;
    let tag_ids = resolve_tag_ids(&names, &tags);

    // Only unknown names means the model ignored the list; don't wipe the tags over it
    if !names.is_empty() && tag_ids.is_empty() {
        return Err(format!(
            "the LLM picked no existing tag (reply: {})",
            names.join(", ")
        ));
    }

    TagsRepository::set_meeting_tags(pool, meeting_id, tag_ids)
        .await
        .map(Some)
        .map_err(|e| format!("failed to save meeting tags: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(id: &str, name: &str, description: Option<&str>) -> TagModel {
        TagModel {
            id: id.to_string(),
            name: name.to_string(),
            color: None,
            description: description.map(str::to_string),
            created_at: String::new(),
            updated_at: String::new(),
            meeting_count: 0,
        }
    }

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn prompts_list_tags_with_descriptions_on_one_line() {
        let tags = vec![
            tag("1", "Payzli", Some("Client meetings\nabout payments")),
            tag("2", "Follow-up", None),
            tag("3", "Internal", Some("   ")),
        ];

        let (system, user) = build_prompts(&tags, "Weekly sync", "## Notes\n- item");

        assert!(system.contains("- Payzli: Client meetings about payments\n"));
        assert!(system.contains("- Follow-up\n"));
        assert!(system.contains("- Internal\n"));
        assert!(system.contains(r#"{"tags": ["Tag name", "Another tag"]}"#));
        assert!(user.starts_with("Meeting title: Weekly sync"));
        assert!(user.contains("<summary>\n## Notes\n- item\n</summary>"));
    }

    #[test]
    fn parses_plain_json_object() {
        assert_eq!(
            parse_tag_names(r#"{"tags": ["Payzli", "Follow-up"]}"#),
            Some(names(&["Payzli", "Follow-up"]))
        );
    }

    #[test]
    fn parses_json_inside_code_fence_and_prose() {
        let reply = "Here are the tags:\n```json\n{\"tags\": [\"Payzli\"]}\n```";
        assert_eq!(parse_tag_names(reply), Some(names(&["Payzli"])));
    }

    #[test]
    fn parses_json_after_thinking_block() {
        let reply = "<think>The meeting is about {payments}</think>\n{\"tags\": [\"Payzli\"]}";
        assert_eq!(parse_tag_names(reply), Some(names(&["Payzli"])));
    }

    #[test]
    fn parses_empty_list() {
        assert_eq!(parse_tag_names(r#"{"tags": []}"#), Some(vec![]));
    }

    #[test]
    fn falls_back_to_bare_array() {
        assert_eq!(
            parse_tag_names(r#"["Payzli", "Internal"]"#),
            Some(names(&["Payzli", "Internal"]))
        );
    }

    #[test]
    fn rejects_unreadable_reply() {
        assert_eq!(parse_tag_names("Payzli, Follow-up"), None);
        assert_eq!(parse_tag_names(r#"{"labels": ["Payzli"]}"#), None);
        assert_eq!(parse_tag_names(""), None);
    }

    #[test]
    fn resolves_names_case_insensitively_without_duplicates() {
        let tags = vec![tag("1", "Payzli", None), tag("2", "Follow-up", None)];

        let ids = resolve_tag_ids(&names(&[" payzli ", "FOLLOW-UP", "Payzli", "Unknown"]), &tags);

        assert_eq!(ids, vec!["1".to_string(), "2".to_string()]);
    }

    #[test]
    fn resolves_nothing_for_unknown_names() {
        let tags = vec![tag("1", "Payzli", None)];
        assert!(resolve_tag_ids(&names(&["Other"]), &tags).is_empty());
    }
}
