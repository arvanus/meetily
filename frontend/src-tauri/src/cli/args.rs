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
    Record(RecordArgs),
    /// List available transcription models and the configured default
    ListModels,
    /// List audio devices
    ListDevices,
}

#[derive(clap::Args, Debug, Default, Clone)]
pub struct RecordArgs {
    /// Meeting name (default: "CLI Meeting <date/time>")
    #[arg(long)]
    pub name: Option<String>,
    /// Record WITHOUT loading AI (minimal RAM); re-transcribe later in the app
    #[arg(long)]
    pub record_only: bool,
    /// Transcription engine (whisper|parakeet); default: the one configured in the app
    #[arg(long)]
    pub engine: Option<String>,
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
}

impl RecordArgs {
    pub fn validate(&self) -> Result<(), String> {
        if self.record_only && self.no_audio_save {
            return Err(
                "--record-only requires saving the audio (it is the source for re-transcription); remove --no-audio-save."
                    .to_string(),
            );
        }
        Ok(())
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
    fn plain_record_only_is_ok() {
        let cli = Cli::parse_from(["meetily-cli", "record", "--record-only"]);
        let Some(Command::Record(args)) = cli.command else { panic!("expected Record") };
        assert!(args.validate().is_ok());
        assert!(args.record_only);
    }
}
