// Speaker names for a meeting transcript: list the speakers, rename one, and ask the
// summary LLM to suggest names from what is said in the meeting.
//
// A rename rewrites the `speaker` column of the meeting's transcripts, so the name
// reaches every reader (transcript view, copy, summary, CLI) without a new table.
// The diarization label it replaces ("SPEAKER_02") is gone afterwards, and running
// diarization or a retranscription again replaces the names.
//
// LLM suggestions are never applied here. Each one must point at transcript lines
// where the name (or a close spelling of it) is actually said, one of them by that
// speaker; anything else is dropped before it reaches the user.

use crate::audio::diarization::{local_user_name, speaker_summary_label, MIC_SPEAKER, SYSTEM_SPEAKER};
use crate::database::repositories::setting::SettingsRepository;
use crate::ollama::metadata::ModelMetadataCache;
use crate::state::AppState;
use crate::summary::context::repository::SummaryContextRepository;
use crate::summary::llm_client::{generate_summary, LLMProvider};
use crate::summary::processor::clean_llm_markdown_detailed;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;
use tauri::{AppHandle, Manager, Runtime};

/// Longest name accepted for a speaker
pub const MAX_SPEAKER_NAME_CHARS: usize = 60;
/// Longest sample line shown for a speaker
const SAMPLE_CHARS: usize = 140;
/// Most evidence lines kept per suggestion
const MAX_EVIDENCE_LINES: usize = 4;
/// Lines repeated at the start of the next chunk, so "Felipe?" / reply pairs are not split
const CHUNK_OVERLAP_LINES: usize = 3;

static METADATA_CACHE: LazyLock<ModelMetadataCache> =
    LazyLock::new(|| ModelMetadataCache::new(Duration::from_secs(300)));

/// One speaker of a meeting, as shown in the Speakers panel
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SpeakerInfo {
    /// Stored value of the `speaker` column ("mic", "SPEAKER_02", "Lucca")
    pub speaker: String,
    /// Display label ("Me", "Speaker 3", "Lucca")
    pub label: String,
    pub segments: u32,
    /// Total speaking time, from the segment timings
    pub seconds: f64,
    /// Longest line said by this speaker, shortened
    pub sample: Option<String>,
    /// False for automatic labels (mic, system, diarization), true once renamed
    pub named: bool,
}

/// A transcript line that backs a suggestion
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EvidenceLine {
    pub time: Option<f64>,
    /// Display label of who says the line
    pub label: Option<String>,
    pub text: String,
}

/// A name the LLM proposes for a speaker; the user accepts, edits or ignores it
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SpeakerNameSuggestion {
    pub speaker: String,
    pub label: String,
    pub name: String,
    /// "high", "medium" or "low"
    pub confidence: String,
    pub reason: String,
    pub evidence: Vec<EvidenceLine>,
}

#[derive(Debug, Clone)]
pub struct TranscriptLine {
    pub speaker: Option<String>,
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub text: String,
}

/// True for labels set by the app (microphone, system audio, diarization), as
/// opposed to a name the user gave
pub fn is_automatic_label(speaker: &str) -> bool {
    if speaker == MIC_SPEAKER || speaker == SYSTEM_SPEAKER {
        return true;
    }
    speaker
        .strip_prefix("SPEAKER_")
        .map_or(false, |n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// Display label for a stored speaker value
pub fn speaker_label(speaker: &str) -> String {
    crate::audio::diarization::speaker_display_name(speaker)
}

/// Trims and checks a new speaker name. Brackets are refused because text exports
/// write speakers as "[Name] text".
pub fn validate_speaker_name(name: &str) -> Result<String, String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err("The name is empty.".to_string());
    }
    if name.chars().count() > MAX_SPEAKER_NAME_CHARS {
        return Err(format!("The name is longer than {} characters.", MAX_SPEAKER_NAME_CHARS));
    }
    if name.chars().any(|c| c == '[' || c == ']' || c.is_control()) {
        return Err("The name cannot contain brackets.".to_string());
    }
    let upper = name.to_uppercase();
    if upper == MIC_SPEAKER.to_uppercase() || upper == SYSTEM_SPEAKER.to_uppercase() || is_automatic_label(&upper) {
        return Err(format!("\"{}\" is reserved for automatic speaker labels.", name));
    }
    Ok(name)
}

fn shorten(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars).collect();
    format!("{}...", cut.trim_end())
}

/// Speakers of a transcript: the microphone first, then by speaking time
pub fn summarize_speakers(lines: &[TranscriptLine]) -> Vec<SpeakerInfo> {
    let mut by_speaker: HashMap<&str, (u32, f64, &str)> = HashMap::new();
    for line in lines {
        let Some(speaker) = line.speaker.as_deref() else { continue };
        let entry = by_speaker.entry(speaker).or_insert((0, 0.0, ""));
        entry.0 += 1;
        if let (Some(start), Some(end)) = (line.start, line.end) {
            if end > start {
                entry.1 += end - start;
            }
        }
        if line.text.trim().chars().count() > entry.2.chars().count() {
            entry.2 = line.text.trim();
        }
    }

    let mut speakers: Vec<SpeakerInfo> = by_speaker
        .into_iter()
        .map(|(speaker, (segments, seconds, sample))| SpeakerInfo {
            speaker: speaker.to_string(),
            label: speaker_label(speaker),
            segments,
            seconds,
            sample: (!sample.is_empty()).then(|| shorten(sample, SAMPLE_CHARS)),
            named: !is_automatic_label(speaker),
        })
        .collect();
    speakers.sort_by(|a, b| {
        (b.speaker == MIC_SPEAKER)
            .cmp(&(a.speaker == MIC_SPEAKER))
            .then(b.seconds.total_cmp(&a.seconds))
            .then(a.label.cmp(&b.label))
    });
    speakers
}

/// Finds a speaker by stored value or display label, ignoring case ("Speaker 3",
/// "SPEAKER_02", "Lucca")
pub fn find_speaker<'a>(speakers: &'a [SpeakerInfo], wanted: &str) -> Option<&'a SpeakerInfo> {
    let wanted = wanted.trim();
    speakers
        .iter()
        .find(|s| s.speaker == wanted)
        .or_else(|| speakers.iter().find(|s| s.label.eq_ignore_ascii_case(wanted) || s.speaker.eq_ignore_ascii_case(wanted)))
}

pub(crate) async fn load_lines(pool: &SqlitePool, meeting_id: &str) -> Result<Vec<TranscriptLine>, String> {
    let rows = sqlx::query_as::<_, (Option<String>, Option<f64>, Option<f64>, String)>(
        "SELECT speaker, audio_start_time, audio_end_time, transcript FROM transcripts
         WHERE meeting_id = ? ORDER BY audio_start_time ASC",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load transcripts: {}", e))?;
    Ok(rows
        .into_iter()
        .map(|(speaker, start, end, text)| TranscriptLine { speaker, start, end, text })
        .collect())
}

pub(crate) async fn list_speakers(pool: &SqlitePool, meeting_id: &str) -> Result<Vec<SpeakerInfo>, String> {
    Ok(summarize_speakers(&load_lines(pool, meeting_id).await?))
}

/// Renames every line of `speaker` in the meeting. Giving two speakers the same name
/// merges them. Returns how many lines changed.
pub(crate) async fn rename_speaker(
    pool: &SqlitePool,
    meeting_id: &str,
    speaker: &str,
    name: &str,
) -> Result<u64, String> {
    if speaker == MIC_SPEAKER {
        return Err("The microphone speaker is you and keeps its label.".to_string());
    }
    let name = validate_speaker_name(name)?;
    if name == speaker {
        return Ok(0);
    }
    let result = sqlx::query("UPDATE transcripts SET speaker = ? WHERE meeting_id = ? AND speaker = ?")
        .bind(&name)
        .bind(meeting_id)
        .bind(speaker)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to rename the speaker: {}", e))?;
    if result.rows_affected() == 0 {
        return Err(format!("No transcript line belongs to speaker \"{}\".", speaker_label(speaker)));
    }
    info!(
        "Renamed speaker {} to {} in meeting {} ({} lines)",
        speaker,
        name,
        meeting_id,
        result.rows_affected()
    );
    Ok(result.rows_affected())
}

fn pool_of<R: Runtime>(app: &AppHandle<R>) -> Result<SqlitePool, String> {
    app.try_state::<AppState>()
        .map(|state| state.db_manager.pool().clone())
        .ok_or_else(|| "App state not available".to_string())
}

#[tauri::command]
pub async fn get_meeting_speakers_command<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
) -> Result<Vec<SpeakerInfo>, String> {
    list_speakers(&pool_of(&app)?, &meeting_id).await
}

#[tauri::command]
pub async fn rename_speaker_command<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    speaker: String,
    name: String,
) -> Result<u64, String> {
    rename_speaker(&pool_of(&app)?, &meeting_id, &speaker, &name).await
}

#[tauri::command]
pub async fn suggest_speaker_names_command<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
) -> Result<Vec<SpeakerNameSuggestion>, String> {
    let app_data_dir = app.path().app_data_dir().ok();
    suggest_speaker_names(&pool_of(&app)?, &meeting_id, app_data_dir.as_ref()).await
}

// ---------------------------------------------------------------------------
// LLM suggestions
// ---------------------------------------------------------------------------

/// The LLM configured for summaries, resolved the same way the summary service does
struct SummaryLlm {
    provider: LLMProvider,
    model: String,
    api_key: String,
    ollama_endpoint: Option<String>,
    custom_openai_endpoint: Option<String>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
}

async fn resolve_summary_llm(pool: &SqlitePool) -> Result<SummaryLlm, String> {
    let setting = match SettingsRepository::get_model_config(pool).await {
        Ok(Some(s)) if !s.provider.is_empty() && !s.model.is_empty() => s,
        _ => return Err("Summary model not configured. Configure it in the settings first.".to_string()),
    };
    let provider = LLMProvider::from_str(&setting.provider)?;

    let mut llm = SummaryLlm {
        provider: provider.clone(),
        model: setting.model.clone(),
        api_key: String::new(),
        ollama_endpoint: None,
        custom_openai_endpoint: None,
        max_tokens: None,
        temperature: None,
        top_p: None,
    };
    match provider {
        LLMProvider::Ollama => llm.ollama_endpoint = setting.ollama_endpoint.clone(),
        LLMProvider::BuiltInAI => {}
        LLMProvider::CustomOpenAI => {
            let config = SettingsRepository::get_custom_openai_config(pool)
                .await
                .map_err(|e| format!("Failed to read the custom OpenAI config: {}", e))?
                .ok_or_else(|| "Custom OpenAI provider selected but no configuration found".to_string())?;
            llm.custom_openai_endpoint = Some(config.endpoint);
            llm.api_key = config.api_key.unwrap_or_default();
            llm.max_tokens = config.max_tokens.map(|t| t as u32);
            llm.temperature = config.temperature;
            llm.top_p = config.top_p;
        }
        _ => {
            llm.api_key = match SettingsRepository::get_api_key(pool, &setting.provider).await {
                Ok(Some(key)) if !key.is_empty() => key,
                Ok(_) => return Err(format!("API key not found for {}", setting.provider)),
                Err(e) => return Err(format!("Failed to read the API key for {}: {}", setting.provider, e)),
            };
        }
    }
    Ok(llm)
}

/// How many transcript characters fit in one request (about 3 characters per token,
/// leaving room for the prompt and the reply)
async fn chunk_char_budget(llm: &SummaryLlm) -> usize {
    let tokens = match llm.provider {
        LLMProvider::Ollama => METADATA_CACHE
            .get_or_fetch(&llm.model, llm.ollama_endpoint.as_deref())
            .await
            .map(|m| m.context_size as usize)
            .unwrap_or(4000),
        LLMProvider::BuiltInAI => crate::summary::summary_engine::models::get_model_by_name(&llm.model)
            .map(|m| m.context_size as usize)
            .unwrap_or(2048),
        _ => 100_000,
    };
    (tokens.saturating_sub(1500) * 3).max(3000)
}

fn format_time(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{:02}:{:02}", m, s)
    }
}

/// Transcript lines as sent to the LLM: "L12 [09:25] [Speaker 3] text"
fn prompt_lines(lines: &[TranscriptLine], user_name: Option<&str>) -> Vec<String> {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let time = line.start.map(|t| format!(" [{}]", format_time(t))).unwrap_or_default();
            let speaker = line
                .speaker
                .as_deref()
                .map(|s| format!(" [{}]", speaker_summary_label(s, user_name)))
                .unwrap_or_default();
            format!("L{}{}{} {}", i, time, speaker, line.text.split_whitespace().collect::<Vec<_>>().join(" "))
        })
        .collect()
}

/// Splits prompt lines into chunks under `budget` characters; each chunk repeats the
/// last lines of the previous one. Returns index ranges into `lines`.
fn chunk_ranges(lines: &[String], budget: usize) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        let mut end = start;
        let mut size = 0;
        while end < lines.len() && (end == start || size + lines[end].len() + 1 <= budget) {
            size += lines[end].len() + 1;
            end += 1;
        }
        ranges.push(start..end);
        if end >= lines.len() {
            break;
        }
        start = end.saturating_sub(CHUNK_OVERLAP_LINES).max(start + 1);
    }
    ranges
}

pub fn build_suggestion_prompt(targets: &[&SpeakerInfo], user_name: Option<&str>, context: Option<&str>) -> String {
    let target_list = targets.iter().map(|s| format!("\"{}\"", s.label)).collect::<Vec<_>>().join(", ");
    let me = match user_name {
        Some(name) => format!("\"Me ({})\" is the local user and is already named. ", name),
        None => "\"Me\" is the local user and is already named. ".to_string(),
    };
    let context_block = match context.map(str::trim).filter(|c| !c.is_empty()) {
        Some(ctx) => format!(
            "\nKnown names and terms (reference only: use them to fix a name that was mis-transcribed, \
             e.g. \"Luca\" for \"Lucca\"; they do not tell who attended the meeting):\n{}\n",
            ctx
        ),
        None => String::new(),
    };
    format!(
        r#"You find out who the unnamed speakers of a meeting transcript are.

Speakers to identify: {targets}. {me}Speaker labels come from automatic speaker detection.

Name a speaker only when the transcript itself shows who they are, for example:
- they introduce themselves ("aqui é o Felipe", "I'm Ana");
- someone addresses them by name and they answer right after;
- someone refers to them by name in a way that makes clear they are the one speaking.
A name that is only mentioned, without tying it to that speaker, is not evidence. A sentence like "Eu, Luca, ..." can be the speaker addressing Luca rather than introducing themselves: use "low" confidence when it is ambiguous. Never guess a name, and leave out every speaker you cannot identify.
{context_block}
Each transcript line starts with an id like L12. For every speaker you identify, list the ids of the lines that prove it, including the line where the name is said.

Respond with only a JSON object and nothing else, in this exact shape:
{{"speakers": [{{"speaker": "Speaker 3", "name": "Lucca", "confidence": "high", "reason": "One short sentence in English.", "evidence": [12, 13]}}]}}
confidence is "high", "medium" or "low". Use {{"speakers": []}} when no speaker can be identified."#,
        targets = target_list,
        me = me,
        context_block = context_block,
    )
}

#[derive(Debug, Deserialize)]
struct SuggestionReply {
    #[serde(default)]
    speakers: Vec<SuggestionItem>,
}

#[derive(Debug, Deserialize)]
struct SuggestionItem {
    speaker: String,
    name: String,
    #[serde(default)]
    confidence: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    evidence: Vec<serde_json::Value>,
}

/// Raw suggestion from one LLM reply, before checking it against the transcript
#[derive(Debug, Clone, PartialEq)]
pub struct RawSuggestion {
    pub speaker: String,
    pub name: String,
    pub confidence: String,
    pub reason: String,
    pub evidence: Vec<usize>,
}

fn line_id(value: &serde_json::Value) -> Option<usize> {
    match value {
        serde_json::Value::Number(n) => n.as_u64().map(|n| n as usize),
        serde_json::Value::String(s) => s.trim().trim_start_matches(['L', 'l']).parse().ok(),
        _ => None,
    }
}

fn normalize_confidence(value: Option<&str>) -> String {
    match value.map(|v| v.trim().to_lowercase()).as_deref() {
        Some("high") => "high".to_string(),
        Some("medium") => "medium".to_string(),
        _ => "low".to_string(),
    }
}

/// Last `{"speakers": ...}` object in `text`. None when there is none, or when the last
/// one does not have the expected shape (an earlier answer may have been retracted).
fn last_reply_object(text: &str) -> Option<SuggestionReply> {
    let mut last = None;
    let mut pos = 0;
    while let Some(offset) = text[pos..].find('{') {
        let start = pos + offset;
        let mut stream = serde_json::Deserializer::from_str(&text[start..]).into_iter::<serde_json::Value>();
        match stream.next() {
            Some(Ok(value)) if value.get("speakers").is_some() => {
                last = Some(serde_json::from_value::<SuggestionReply>(value).ok());
                pos = start + stream.byte_offset();
            }
            _ => pos = start + 1,
        }
    }
    last.flatten()
}

/// Reads `{"speakers": [...]}` out of an LLM reply (thinking blocks, fences and prose
/// around it are ignored). A model that corrects itself writes several of them; the
/// last one is its answer. None when nothing parses.
pub fn parse_suggestion_reply(response: &str) -> Option<Vec<RawSuggestion>> {
    let cleaned = clean_llm_markdown_detailed(response).markdown;
    let reply = last_reply_object(&cleaned)?;
    Some(
        reply
            .speakers
            .into_iter()
            .map(|item| RawSuggestion {
                speaker: item.speaker.trim().to_string(),
                name: item.name.trim().to_string(),
                confidence: normalize_confidence(item.confidence.as_deref()),
                reason: item.reason.unwrap_or_default().trim().to_string(),
                evidence: item.evidence.iter().filter_map(line_id).collect(),
            })
            .collect(),
    )
}

/// Lowercase without accents, for comparing a name with what was transcribed
fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (previous[j] + cost).min(previous[j + 1] + 1).min(current[j] + 1);
        }
        previous = current;
    }
    previous[b.len()]
}

/// True when the first word of `name` appears in `text`, or a close spelling of it
/// ("Luca" for "Lucca", "Filipe" for "Felipe") written with a capital letter, so common
/// words like "digo" (Diego) or "na" (Ana) do not count
pub fn name_is_spoken(name: &str, text: &str) -> bool {
    let Some(first) = fold(name).split_whitespace().next().map(|w| w.chars().collect::<Vec<_>>()) else {
        return false;
    };
    let allowed = if first.len() <= 4 { 1 } else { 2 };
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .any(|word| {
            let folded: Vec<char> = fold(word).chars().collect();
            // shortcut: a capitalized common word at the start of a sentence ("Digo, ...") still
            // matches; if that shows up in real meetings, skip the first word of each sentence
            folded == first
                || (word.starts_with(char::is_uppercase)
                    && folded.len() + allowed >= first.len()
                    && first.len() + allowed >= folded.len()
                    && edit_distance(&first, &folded) <= allowed)
        })
}

/// Keeps the suggestions the transcript backs up: a known unnamed speaker, a valid
/// name, evidence lines that exist, the name said in at least one of them and at least
/// one of them said by that speaker.
/// Several replies (one per chunk) are merged: per speaker, the name with the most
/// evidence wins and disagreement lowers the confidence.
pub fn validate_suggestions(
    raw: &[RawSuggestion],
    targets: &[&SpeakerInfo],
    lines: &[TranscriptLine],
) -> Vec<SpeakerNameSuggestion> {
    // speaker -> folded name -> (display name, confidence, reasons, evidence ids)
    let mut grouped: BTreeMap<String, BTreeMap<String, (String, String, Vec<String>, Vec<usize>)>> = BTreeMap::new();

    for suggestion in raw {
        let Some(target) = targets.iter().find(|t| {
            t.speaker == suggestion.speaker
                || t.label.eq_ignore_ascii_case(&suggestion.speaker)
                || t.speaker.eq_ignore_ascii_case(&suggestion.speaker)
        }) else {
            continue;
        };
        let Ok(name) = validate_speaker_name(&suggestion.name) else { continue };

        let mut seen = HashSet::new();
        let evidence: Vec<usize> = suggestion
            .evidence
            .iter()
            .copied()
            .filter(|id| *id < lines.len() && seen.insert(*id))
            .collect();
        if !evidence.iter().any(|id| name_is_spoken(&name, &lines[*id].text)) {
            continue;
        }
        // The speaker must say one of the lines: introducing themselves, or answering when named
        if !evidence.iter().any(|id| lines[*id].speaker.as_deref() == Some(target.speaker.as_str())) {
            continue;
        }

        let entry = grouped
            .entry(target.speaker.clone())
            .or_default()
            .entry(fold(&name))
            .or_insert_with(|| (name.clone(), "low".to_string(), Vec::new(), Vec::new()));
        if confidence_rank(&suggestion.confidence) > confidence_rank(&entry.1) {
            entry.1 = suggestion.confidence.clone();
        }
        if !suggestion.reason.is_empty() && !entry.2.contains(&suggestion.reason) {
            entry.2.push(suggestion.reason.clone());
        }
        for id in evidence {
            if !entry.3.contains(&id) {
                entry.3.push(id);
            }
        }
    }

    let user_name = local_user_name();
    let mut suggestions = Vec::new();
    for target in targets {
        let Some(candidates) = grouped.remove(&target.speaker) else { continue };
        let mut candidates: Vec<_> = candidates.into_values().collect();
        candidates.sort_by(|a, b| {
            b.3.len()
                .cmp(&a.3.len())
                .then(confidence_rank(&b.1).cmp(&confidence_rank(&a.1)))
        });
        let (name, mut confidence, reasons, mut evidence) = candidates[0].clone();
        let mut reason = reasons.join(" ");
        if candidates.len() > 1 {
            confidence = "low".to_string();
            let others = candidates[1..].iter().map(|c| c.0.as_str()).collect::<Vec<_>>().join(", ");
            reason = format!("{} Other candidates: {}.", reason, others).trim().to_string();
        }
        evidence.sort_unstable();
        evidence.truncate(MAX_EVIDENCE_LINES);
        suggestions.push(SpeakerNameSuggestion {
            speaker: target.speaker.clone(),
            label: target.label.clone(),
            name,
            confidence,
            reason,
            evidence: evidence
                .into_iter()
                .map(|id| EvidenceLine {
                    time: lines[id].start,
                    label: lines[id]
                        .speaker
                        .as_deref()
                        .map(|s| speaker_summary_label(s, user_name.as_deref())),
                    text: lines[id].text.trim().to_string(),
                })
                .collect(),
        });
    }
    suggestions
}

fn confidence_rank(confidence: &str) -> u8 {
    match confidence {
        "high" => 2,
        "medium" => 1,
        _ => 0,
    }
}

/// Reference context for names: the meeting's template context and its own context text
async fn naming_context(pool: &SqlitePool, meeting_id: &str) -> Option<String> {
    let mut parts = Vec::new();
    if let Ok(Some(template_id)) = SummaryContextRepository::get_template(pool, meeting_id).await {
        if let Some(ctx) = crate::summary::templates::get_template(&template_id).ok().and_then(|t| t.context) {
            parts.push(ctx);
        }
    }
    if let Ok(prompt) = SummaryContextRepository::get_prompt(pool, meeting_id).await {
        parts.push(prompt);
    }
    let joined = parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.is_empty()).then_some(joined)
}

/// Asks the summary LLM who the unnamed speakers are. Long transcripts are sent in
/// chunks and the answers merged. Nothing is renamed here.
pub(crate) async fn suggest_speaker_names(
    pool: &SqlitePool,
    meeting_id: &str,
    app_data_dir: Option<&PathBuf>,
) -> Result<Vec<SpeakerNameSuggestion>, String> {
    let lines = load_lines(pool, meeting_id).await?;
    if lines.is_empty() {
        return Err("This meeting has no transcript.".to_string());
    }
    let speakers = summarize_speakers(&lines);
    let targets: Vec<&SpeakerInfo> = speakers
        .iter()
        .filter(|s| !s.named && s.speaker != MIC_SPEAKER)
        .collect();
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let llm = resolve_summary_llm(pool).await?;
    let user_name = local_user_name();
    let context = naming_context(pool, meeting_id).await;
    let system_prompt = build_suggestion_prompt(&targets, user_name.as_deref(), context.as_deref());
    let rendered = prompt_lines(&lines, user_name.as_deref());
    let ranges = chunk_ranges(&rendered, chunk_char_budget(&llm).await);
    info!(
        "Suggesting speaker names for meeting {}: {} speakers, {} lines, {} request(s) to {:?}/{}",
        meeting_id,
        targets.len(),
        lines.len(),
        ranges.len(),
        llm.provider,
        llm.model
    );

    let client = reqwest::Client::new();
    let mut raw = Vec::new();
    let mut failures = 0;
    for range in &ranges {
        let user_prompt = format!("<transcript>\n{}\n</transcript>", rendered[range.clone()].join("\n"));
        let reply = generate_summary(
            &client,
            &llm.provider,
            &llm.model,
            &llm.api_key,
            &system_prompt,
            &user_prompt,
            llm.ollama_endpoint.as_deref(),
            llm.custom_openai_endpoint.as_deref(),
            llm.max_tokens,
            llm.temperature,
            llm.top_p,
            app_data_dir,
            None,
        )
        .await?
        .content;
        match parse_suggestion_reply(&reply) {
            Some(parsed) => raw.extend(parsed),
            None => {
                failures += 1;
                warn!("Could not read speaker suggestions from the LLM reply: {}", shorten(&reply, 300));
            }
        }
    }
    if failures == ranges.len() {
        return Err("The LLM reply could not be read. Try again or use another model.".to_string());
    }

    let suggestions = validate_suggestions(&raw, &targets, &lines);
    info!(
        "Speaker name suggestions for meeting {}: {} from the LLM, {} backed by the transcript",
        meeting_id,
        raw.len(),
        suggestions.len()
    );
    Ok(suggestions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(speaker: &str, start: f64, end: f64, text: &str) -> TranscriptLine {
        TranscriptLine {
            speaker: Some(speaker.to_string()),
            start: Some(start),
            end: Some(end),
            text: text.to_string(),
        }
    }

    fn raw(speaker: &str, name: &str, confidence: &str, evidence: &[usize]) -> RawSuggestion {
        RawSuggestion {
            speaker: speaker.to_string(),
            name: name.to_string(),
            confidence: confidence.to_string(),
            reason: String::new(),
            evidence: evidence.to_vec(),
        }
    }

    fn sample_lines() -> Vec<TranscriptLine> {
        vec![
            line("mic", 0.0, 4.0, "Beleza, quem vai agora?"),
            line("SPEAKER_02", 5.0, 9.0, "Eu, Luca, foquei ali no projeto de campanhas."),
            line("mic", 10.0, 12.0, "Felipe, e o teu lado?"),
            line("SPEAKER_01", 12.5, 20.0, "Então, eu terminei a integração."),
            line("SPEAKER_01", 21.0, 22.0, "Ok."),
        ]
    }

    #[test]
    fn automatic_labels_are_recognized() {
        assert!(is_automatic_label("mic"));
        assert!(is_automatic_label("system"));
        assert!(is_automatic_label("SPEAKER_00"));
        assert!(!is_automatic_label("SPEAKER_"));
        assert!(!is_automatic_label("Lucca"));
    }

    #[test]
    fn names_are_trimmed_and_checked() {
        assert_eq!(validate_speaker_name("  Ana   Paula ").unwrap(), "Ana Paula");
        assert!(validate_speaker_name("   ").is_err());
        assert!(validate_speaker_name("[Ana]").is_err());
        assert!(validate_speaker_name("mic").is_err());
        assert!(validate_speaker_name("System").is_err());
        assert!(validate_speaker_name("speaker_03").is_err());
        assert!(validate_speaker_name(&"a".repeat(MAX_SPEAKER_NAME_CHARS + 1)).is_err());
    }

    #[test]
    fn speakers_are_summarized_mic_first_then_by_time() {
        let speakers = summarize_speakers(&sample_lines());
        let order: Vec<&str> = speakers.iter().map(|s| s.speaker.as_str()).collect();
        assert_eq!(order, vec!["mic", "SPEAKER_01", "SPEAKER_02"]);
        let s1 = &speakers[1];
        assert_eq!(s1.label, "Speaker 2");
        assert_eq!(s1.segments, 2);
        assert!((s1.seconds - 8.5).abs() < 1e-9);
        assert_eq!(s1.sample.as_deref(), Some("Então, eu terminei a integração."));
        assert!(!s1.named);
    }

    #[test]
    fn find_speaker_accepts_label_or_stored_value() {
        let speakers = summarize_speakers(&sample_lines());
        assert_eq!(find_speaker(&speakers, "speaker 3").unwrap().speaker, "SPEAKER_02");
        assert_eq!(find_speaker(&speakers, "SPEAKER_01").unwrap().speaker, "SPEAKER_01");
        assert!(find_speaker(&speakers, "Nobody").is_none());
    }

    #[test]
    fn spoken_name_tolerates_transcription_errors() {
        assert!(name_is_spoken("Lucca", "Eu, Luca, foquei ali"));
        assert!(name_is_spoken("Felipe", "Filipe, e o teu lado?"));
        assert!(name_is_spoken("José Silva", "obrigado jose"));
        assert!(!name_is_spoken("Diego", "Eu, Luca, foquei ali"));
        assert!(!name_is_spoken("Ana", "banana e campanha"));
    }

    #[test]
    fn spoken_name_ignores_common_words_close_to_the_name() {
        assert!(!name_is_spoken("Diego", "Como eu digo, isso vale"));
        assert!(!name_is_spoken("Ana", "Vamos ver na reunião"));
        assert!(name_is_spoken("Ana", "ana, pode falar"), "an exact match needs no capital letter");
    }

    #[test]
    fn suggestions_need_a_line_by_the_named_speaker() {
        let lines = sample_lines();
        let speakers = summarize_speakers(&lines);
        let targets: Vec<&SpeakerInfo> = speakers.iter().filter(|s| s.speaker != "mic").collect();
        // Line 1 is Speaker 3 saying "Eu, Luca": it proves nothing about Speaker 2
        let suggestions = validate_suggestions(&[raw("Speaker 2", "Lucca", "high", &[1])], &targets, &lines);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn suggestions_need_the_name_in_existing_evidence() {
        let lines = sample_lines();
        let speakers = summarize_speakers(&lines);
        let targets: Vec<&SpeakerInfo> = speakers.iter().filter(|s| s.speaker != "mic").collect();

        let suggestions = validate_suggestions(
            &[
                raw("Speaker 3", "Lucca", "medium", &[1]),
                // Felipe is said on line 2 (by mic) and answered on line 3
                raw("Speaker 2", "Felipe", "high", &[2, 3, 99]),
                // Diego is never said: dropped
                raw("Speaker 3", "Diego", "high", &[1]),
                // unknown speaker and the mic are not targets
                raw("Speaker 9", "Ana", "high", &[0]),
                raw("Me", "Lucas", "high", &[0]),
            ],
            &targets,
            &lines,
        );

        assert_eq!(suggestions.len(), 2);
        let felipe = suggestions.iter().find(|s| s.speaker == "SPEAKER_01").unwrap();
        assert_eq!(felipe.name, "Felipe");
        assert_eq!(felipe.confidence, "high");
        assert_eq!(felipe.evidence.len(), 2, "line ids outside the transcript are dropped");
        assert_eq!(felipe.evidence[0].text, "Felipe, e o teu lado?");
        let lucca = suggestions.iter().find(|s| s.speaker == "SPEAKER_02").unwrap();
        assert_eq!(lucca.name, "Lucca");
        assert_eq!(lucca.confidence, "medium", "Diego was dropped, so there is no conflict");
    }

    #[test]
    fn conflicting_chunks_keep_the_best_backed_name_with_low_confidence() {
        let lines = vec![
            line("SPEAKER_00", 0.0, 1.0, "Aqui é o Bruno."),
            line("SPEAKER_00", 1.0, 2.0, "Bruno de novo."),
            line("SPEAKER_00", 2.0, 3.0, "Valeu, Carla."),
        ];
        let speakers = summarize_speakers(&lines);
        let targets: Vec<&SpeakerInfo> = speakers.iter().collect();
        let suggestions = validate_suggestions(
            &[
                raw("Speaker 1", "Bruno", "high", &[0, 1]),
                raw("Speaker 1", "Carla", "medium", &[2]),
            ],
            &targets,
            &lines,
        );
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].name, "Bruno");
        assert_eq!(suggestions[0].confidence, "low");
        assert!(suggestions[0].reason.contains("Carla"));
    }

    #[test]
    fn reply_parsing_accepts_noise_and_string_ids() {
        let reply = "<think>hmm</think>\n```json\n{\"speakers\": [{\"speaker\": \"Speaker 3\", \"name\": \" Lucca \", \"confidence\": \"Medium\", \"reason\": \"Says Luca.\", \"evidence\": [\"L12\", 13, \"x\"]}]}\n```";
        let parsed = parse_suggestion_reply(reply).unwrap();
        assert_eq!(parsed, vec![RawSuggestion {
            speaker: "Speaker 3".to_string(),
            name: "Lucca".to_string(),
            confidence: "medium".to_string(),
            reason: "Says Luca.".to_string(),
            evidence: vec![12, 13],
        }]);
        assert_eq!(parse_suggestion_reply("{\"speakers\": []}").unwrap(), vec![]);
        assert!(parse_suggestion_reply("no idea").is_none());
    }

    #[test]
    fn reply_parsing_keeps_the_last_answer_when_the_model_corrects_itself() {
        // Shape of a real claude-sonnet-4-6 reply: an answer, a retraction, then the final answer
        let reply = "```json\n{\"speakers\": [{\"speaker\": \"Speaker 4\", \"name\": \"Fernando\", \"confidence\": \"low\", \"reason\": \"Fits as Fernando reacting.\", \"evidence\": [0, 80]}]}\n```\n\nWait, let me re-examine more carefully.\n\nAt L0, Speaker 4 says \"Muito bem.\" {not json} and is never named.\n\n```json\n{\"speakers\": []}\n```";
        assert_eq!(parse_suggestion_reply(reply).unwrap(), vec![]);

        let reply = "{\"speakers\": []}\nOn second thought:\n{\"speakers\": [{\"speaker\": \"Speaker 2\", \"name\": \"Felipe\", \"evidence\": [3]}]}";
        let parsed = parse_suggestion_reply(reply).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "Felipe");
    }

    #[test]
    fn chunks_overlap_and_cover_every_line() {
        let lines: Vec<String> = (0..20).map(|i| format!("L{} {}", i, "x".repeat(40))).collect();
        let ranges = chunk_ranges(&lines, 200);
        assert!(ranges.len() > 1);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges.last().unwrap().end, lines.len());
        for pair in ranges.windows(2) {
            assert!(pair[1].start < pair[0].end, "consecutive chunks must overlap");
            assert!(pair[1].start > pair[0].start, "chunks must advance");
        }
        assert_eq!(chunk_ranges(&lines, 1_000_000), vec![0..20]);
    }

    #[test]
    fn prompt_lists_targets_and_marks_context_as_reference() {
        let lines = sample_lines();
        let speakers = summarize_speakers(&lines);
        let targets: Vec<&SpeakerInfo> = speakers.iter().filter(|s| s.speaker != "mic").collect();
        let prompt = build_suggestion_prompt(&targets, Some("Lucas"), Some("Team: Lucca, Felipe"));
        assert!(prompt.contains("\"Speaker 2\", \"Speaker 3\""));
        assert!(prompt.contains("\"Me (Lucas)\" is the local user"));
        assert!(prompt.contains("reference only"));
        assert!(prompt.contains("Team: Lucca, Felipe"));
        assert!(prompt.contains("Never guess"));

        let rendered = prompt_lines(&lines, Some("Lucas"));
        assert_eq!(rendered[1], "L1 [00:05] [Speaker 3] Eu, Luca, foquei ali no projeto de campanhas.");
        assert_eq!(rendered[0], "L0 [00:00] [Me (Lucas)] Beleza, quem vai agora?");
    }
}
