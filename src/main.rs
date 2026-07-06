mod diff_model;
mod github;
mod state;
mod tui;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

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
    let cli = Cli::parse();

    match cli.command {
        Commands::Review { pr, repo, db } => {
            let gh = github::Gh::new(repo);
            let pr_meta = gh.pr_view(pr.as_deref())?;
            let root = state::state_root(db)?;
            let mut session = state::ReviewSession::load(root, pr_meta)?;
            session.refresh_files(&gh)?;
            tui::run(session, gh)?;
        }
    }

    Ok(())
}
