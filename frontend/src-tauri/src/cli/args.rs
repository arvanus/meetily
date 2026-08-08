use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "meetily-cli", about = "Record and transcribe meetings without the UI")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Record a meeting (default subcommand)
    ///
    /// Hotkeys while recording: press 'p' to pause/resume, Ctrl+C (or 'q') to
    /// stop and save.
    Record(RecordArgs),
    /// List available transcription models and the configured default
    ListModels,
    /// List audio devices
    ListDevices,
    /// List meetings (id, date, title) to discover meeting ids
    ListMeetings,
    /// List available summary templates (id, name, description)
    ListTemplates,
    /// Re-transcribe an existing meeting's saved audio with another model
    Retranscribe(RetranscribeArgs),
    /// Generate the summary of a meeting (reuses the app's summary pipeline)
    Summarize(SummarizeArgs),
}

#[derive(clap::Args, Debug, Default, Clone)]
pub struct RecordArgs {
    /// Meeting name (default: "CLI Meeting <date/time>")
    #[arg(long)]
    pub name: Option<String>,
    /// Seed details/observations for the AI summary context; while recording press
    /// 'n' to append more lines live. Saved to the meeting on stop.
    #[arg(long)]
    pub notes: Option<String>,
    /// Record WITHOUT loading AI (minimal RAM); re-transcribe later in the app
    #[arg(long)]
    pub record_only: bool,
    /// Transcription engine (whisper|parakeet); default: the one configured in the app
    #[arg(long)]
    pub engine: Option<String>,
    /// Transcription language code passed through to the engine (e.g. "pt", "en", "auto");
    /// used by Whisper. Default: the app preference ("auto-translate" = translate to English)
    #[arg(long)]
    pub language: Option<String>,
    /// Model name; default: the one configured in the app
    #[arg(long)]
    pub model: Option<String>,
    /// Microphone device (default: OS default)
    #[arg(long)]
    pub mic: Option<String>,
    /// System audio device (default: OS loopback)
    #[arg(long)]
    pub system: Option<String>,
    /// Show live partial transcriptions
    #[arg(long)]
    pub partial: bool,
    /// Do not print transcripts (the live panel stays on)
    #[arg(long)]
    pub quiet: bool,
    /// Stop automatically after N seconds
    #[arg(long)]
    pub duration: Option<u64>,
    /// Transcribe only, do not save the audio file (incompatible with --record-only)
    #[arg(long)]
    pub no_audio_save: bool,
    /// After saving, re-transcribe the recorded audio with this model (usually a
    /// bigger/better one than the live model) before summarizing. The meeting id comes
    /// from this same run, so overlapping CLI processes never touch each other.
    #[arg(long)]
    pub retranscribe_model: Option<String>,
    /// Engine used by --retranscribe-model (whisper|parakeet); default: --engine, else whisper
    #[arg(long)]
    pub retranscribe_engine: Option<String>,
    /// Language code used by --retranscribe-model (e.g. "pt", "en"); default: --language
    #[arg(long)]
    pub retranscribe_language: Option<String>,
    /// After saving, generate the summary of the just-recorded meeting
    #[arg(long)]
    pub summarize: bool,
    /// Template id used when --summarize is set (default: "daily_standup")
    #[arg(long)]
    pub template: Option<String>,
}

impl RecordArgs {
    pub fn validate(&self) -> Result<(), String> {
        if self.record_only && self.no_audio_save {
            return Err(
                "--record-only requires saving the audio (it is the source for re-transcription); remove --no-audio-save."
                    .to_string(),
            );
        }
        if self.retranscribe_model.is_some() && self.no_audio_save {
            return Err(
                "--retranscribe-model reads the saved audio file; remove --no-audio-save."
                    .to_string(),
            );
        }
        if (self.retranscribe_engine.is_some() || self.retranscribe_language.is_some())
            && self.retranscribe_model.is_none()
        {
            return Err(
                "--retranscribe-engine/--retranscribe-language require --retranscribe-model."
                    .to_string(),
            );
        }
        // --summarize needs a transcript. --record-only produces none by itself, but
        // --retranscribe-model creates one from the saved audio, so the combination
        // record-only + retranscribe + summarize is valid (and is the cheapest path:
        // no AI while recording, a single pass with the good model afterwards).
        if self.summarize && self.record_only && self.retranscribe_model.is_none() {
            return Err(
                "--summarize needs a transcription; with --record-only, add --retranscribe-model <model>."
                    .to_string(),
            );
        }
        Ok(())
    }
}

#[derive(clap::Args, Debug, Default, Clone)]
pub struct RetranscribeArgs {
    /// Meeting id to re-transcribe (use `list-meetings` to discover ids). Prefer this
    /// over --last when several CLI runs can overlap.
    #[arg(long)]
    pub meeting: Option<String>,
    /// Re-transcribe the most recent meeting instead of passing an id
    #[arg(long)]
    pub last: bool,
    /// Model to transcribe with (see `list-models`)
    #[arg(long)]
    pub model: String,
    /// Engine (whisper|parakeet); default: whisper
    #[arg(long)]
    pub engine: Option<String>,
    /// Language code (e.g. "pt", "en"); default: the app preference
    #[arg(long)]
    pub language: Option<String>,
    /// After re-transcribing, generate the summary of the meeting
    #[arg(long)]
    pub summarize: bool,
    /// Template id used when --summarize is set (default: "daily_standup")
    #[arg(long)]
    pub template: Option<String>,
}

impl RetranscribeArgs {
    pub fn validate(&self) -> Result<(), String> {
        match (self.meeting.is_some(), self.last) {
            (true, true) => Err("use either --meeting <id> or --last, not both.".to_string()),
            (false, false) => {
                Err("specify the meeting to re-transcribe: --meeting <id> or --last.".to_string())
            }
            _ => Ok(()),
        }
    }
}

#[derive(clap::Args, Debug, Default, Clone)]
pub struct SummarizeArgs {
    /// Meeting id to summarize (use `list-meetings` to discover ids)
    #[arg(long)]
    pub meeting: Option<String>,
    /// Summarize the most recent meeting instead of passing an id
    #[arg(long)]
    pub last: bool,
    /// Template id (default: "daily_standup"); see `list-templates`
    #[arg(long)]
    pub template: Option<String>,
}

impl SummarizeArgs {
    pub fn validate(&self) -> Result<(), String> {
        match (self.meeting.is_some(), self.last) {
            (true, true) => {
                Err("use either --meeting <id> or --last, not both.".to_string())
            }
            (false, false) => {
                Err("specify the meeting to summarize: --meeting <id> or --last.".to_string())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn record_only_with_no_audio_save_is_rejected() {
        let cli = Cli::parse_from(["meetily-cli", "record", "--record-only", "--no-audio-save"]);
        let Some(Command::Record(args)) = cli.command else { panic!("expected Record") };
        assert!(args.validate().is_err());
    }

    #[test]
    fn language_is_passed_through() {
        let cli = Cli::parse_from(["meetily-cli", "record", "--language", "pt"]);
        let Some(Command::Record(args)) = cli.command else { panic!("expected Record") };
        assert_eq!(args.language.as_deref(), Some("pt"));
    }

    #[test]
    fn notes_flag_is_parsed() {
        let cli = Cli::parse_from(["meetily-cli", "record", "--notes", "kickoff call"]);
        let Some(Command::Record(args)) = cli.command else { panic!("expected Record") };
        assert_eq!(args.notes.as_deref(), Some("kickoff call"));
    }

    #[test]
    fn plain_record_only_is_ok() {
        let cli = Cli::parse_from(["meetily-cli", "record", "--record-only"]);
        let Some(Command::Record(args)) = cli.command else { panic!("expected Record") };
        assert!(args.validate().is_ok());
        assert!(args.record_only);
    }

    #[test]
    fn summarize_requires_a_target() {
        let cli = Cli::parse_from(["meetily-cli", "summarize"]);
        let Some(Command::Summarize(args)) = cli.command else { panic!("expected Summarize") };
        assert!(args.validate().is_err());
    }

    #[test]
    fn summarize_rejects_both_targets() {
        let cli = Cli::parse_from(["meetily-cli", "summarize", "--meeting", "x", "--last"]);
        let Some(Command::Summarize(args)) = cli.command else { panic!("expected Summarize") };
        assert!(args.validate().is_err());
    }

    #[test]
    fn summarize_last_is_ok() {
        let cli = Cli::parse_from(["meetily-cli", "summarize", "--last"]);
        let Some(Command::Summarize(args)) = cli.command else { panic!("expected Summarize") };
        assert!(args.validate().is_ok());
        assert!(args.last);
    }

    #[test]
    fn summarize_meeting_id_is_ok() {
        let cli = Cli::parse_from(["meetily-cli", "summarize", "--meeting", "abc"]);
        let Some(Command::Summarize(args)) = cli.command else { panic!("expected Summarize") };
        assert!(args.validate().is_ok());
        assert_eq!(args.meeting.as_deref(), Some("abc"));
    }

    #[test]
    fn record_summarize_with_record_only_is_rejected() {
        let cli = Cli::parse_from(["meetily-cli", "record", "--summarize", "--record-only"]);
        let Some(Command::Record(args)) = cli.command else { panic!("expected Record") };
        assert!(args.validate().is_err());
    }

    #[test]
    fn record_summarize_alone_is_ok() {
        let cli = Cli::parse_from(["meetily-cli", "record", "--summarize"]);
        let Some(Command::Record(args)) = cli.command else { panic!("expected Record") };
        assert!(args.validate().is_ok());
        assert!(args.summarize);
    }

    #[test]
    fn retranscribe_model_is_parsed() {
        let cli = Cli::parse_from([
            "meetily-cli",
            "record",
            "--model",
            "base",
            "--retranscribe-model",
            "large-v3",
            "--summarize",
        ]);
        let Some(Command::Record(args)) = cli.command else { panic!("expected Record") };
        assert!(args.validate().is_ok());
        assert_eq!(args.model.as_deref(), Some("base"));
        assert_eq!(args.retranscribe_model.as_deref(), Some("large-v3"));
    }

    #[test]
    fn retranscribe_with_no_audio_save_is_rejected() {
        let cli = Cli::parse_from([
            "meetily-cli",
            "record",
            "--retranscribe-model",
            "large-v3",
            "--no-audio-save",
        ]);
        let Some(Command::Record(args)) = cli.command else { panic!("expected Record") };
        assert!(args.validate().is_err());
    }

    #[test]
    fn retranscribe_engine_without_model_is_rejected() {
        let cli = Cli::parse_from(["meetily-cli", "record", "--retranscribe-engine", "parakeet"]);
        let Some(Command::Record(args)) = cli.command else { panic!("expected Record") };
        assert!(args.validate().is_err());
    }

    #[test]
    fn record_only_with_retranscribe_and_summarize_is_ok() {
        let cli = Cli::parse_from([
            "meetily-cli",
            "record",
            "--record-only",
            "--retranscribe-model",
            "large-v3",
            "--summarize",
        ]);
        let Some(Command::Record(args)) = cli.command else { panic!("expected Record") };
        assert!(args.validate().is_ok());
    }

    #[test]
    fn record_only_summarize_without_retranscribe_is_rejected() {
        let cli = Cli::parse_from(["meetily-cli", "record", "--record-only", "--summarize"]);
        let Some(Command::Record(args)) = cli.command else { panic!("expected Record") };
        assert!(args.validate().is_err());
    }

    #[test]
    fn retranscribe_meeting_id_with_summarize_is_ok() {
        let cli = Cli::parse_from([
            "meetily-cli",
            "retranscribe",
            "--meeting",
            "abc",
            "--model",
            "large-v3",
            "--summarize",
        ]);
        let Some(Command::Retranscribe(args)) = cli.command else {
            panic!("expected Retranscribe")
        };
        assert!(args.validate().is_ok());
        assert_eq!(args.meeting.as_deref(), Some("abc"));
        assert_eq!(args.model, "large-v3");
        assert!(args.summarize);
    }

    #[test]
    fn retranscribe_requires_a_target() {
        let cli = Cli::parse_from(["meetily-cli", "retranscribe", "--model", "large-v3"]);
        let Some(Command::Retranscribe(args)) = cli.command else {
            panic!("expected Retranscribe")
        };
        assert!(args.validate().is_err());
    }

    #[test]
    fn retranscribe_rejects_both_targets() {
        let cli = Cli::parse_from([
            "meetily-cli",
            "retranscribe",
            "--meeting",
            "abc",
            "--last",
            "--model",
            "large-v3",
        ]);
        let Some(Command::Retranscribe(args)) = cli.command else {
            panic!("expected Retranscribe")
        };
        assert!(args.validate().is_err());
    }
}
