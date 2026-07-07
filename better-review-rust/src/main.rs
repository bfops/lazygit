mod diff_model;
mod github;
mod jobs;
mod log_buffer;
mod prefs;
mod state;
mod tui;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::fmt::format::FmtSpan;

#[derive(Debug, Parser)]
#[command(name = "better-review")]
#[command(about = "Review GitHub PRs from reviewed file state")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Review {
        pr: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        db: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_span_events(FmtSpan::NONE)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Review { pr, repo, db } => {
            let logs = log_buffer::LogBuffer::new(200);
            let prefs = prefs::Preferences::load()?;
            logs.record_stderr(format!(
                "resolving PR {}",
                pr.as_deref().unwrap_or("for current branch")
            ));
            let gh = github::Gh::new(repo);
            let pr_meta = gh.pr_view(pr.as_deref())?;
            logs.record_stderr(format!(
                "resolved PR #{} with {} changed files",
                pr_meta.number,
                pr_meta.files.len()
            ));
            let root = state::state_root(db)?;
            logs.record_stderr(format!("using state directory {}", root.display()));
            let mut session = state::ReviewSession::load(root, pr_meta)?;
            logs.set_terminal_logging_enabled(true);
            logs.record_stderr("refreshing file contents from GitHub");
            session.refresh_files_with_logs(&gh, Some(&logs))?;
            logs.record_stderr("entering TUI");
            logs.set_terminal_logging_enabled(false);
            tui::run(session, gh, prefs, logs)?;
        }
    }

    Ok(())
}
