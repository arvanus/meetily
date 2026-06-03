use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "meetily-cli", about = "Grava e transcreve reuniões sem a UI")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Grava uma reunião (subcomando padrão)
    Record(RecordArgs),
    /// Lista os modelos de transcrição disponíveis e o padrão
    ListModels,
    /// Lista os dispositivos de áudio
    ListDevices,
}

#[derive(clap::Args, Debug, Default, Clone)]
pub struct RecordArgs {
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub record_only: bool,
    #[arg(long)]
    pub engine: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub mic: Option<String>,
    #[arg(long)]
    pub system: Option<String>,
    #[arg(long)]
    pub partial: bool,
    #[arg(long)]
    pub quiet: bool,
    #[arg(long)]
    pub duration: Option<u64>,
    #[arg(long)]
    pub no_audio_save: bool,
}

impl RecordArgs {
    pub fn validate(&self) -> Result<(), String> {
        if self.record_only && self.no_audio_save {
            return Err(
                "--record-only requer salvar o áudio (é a fonte da re-transcrição); remova --no-audio-save."
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
        let Some(Command::Record(args)) = cli.command else { panic!("esperava Record") };
        assert!(args.validate().is_err());
    }

    #[test]
    fn plain_record_only_is_ok() {
        let cli = Cli::parse_from(["meetily-cli", "record", "--record-only"]);
        let Some(Command::Record(args)) = cli.command else { panic!("esperava Record") };
        assert!(args.validate().is_ok());
        assert!(args.record_only);
    }
}
