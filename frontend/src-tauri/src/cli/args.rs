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
        if self.summarize && self.record_only {
            return Err(
                "--summarize needs a transcription; it is incompatible with --record-only."
                    .to_string(),
            );
        }
        Ok(())
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
}
