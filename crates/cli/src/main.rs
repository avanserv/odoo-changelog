mod walk;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use odoo_changelog_core::Config;
use odoo_changelog_store::Store;

#[derive(Parser)]
#[command(version, about = "Symbol-level changelog of Odoo")]
struct Cli {
    #[arg(long, short, default_value = "config/odoo-changelog.toml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Resolve version forks and record every new commit of every configured repo.
    Walk {
        /// Only these repos (by config name).
        #[arg(long)]
        repo: Vec<String>,
    },
    /// Commit counts per repo and version.
    Stats,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let cfg = Config::load(&cli.config)?;
    let store = Store::open(&cfg.db)?;
    match cli.command {
        Command::Walk { repo } => walk::run(&cfg, &store, &repo),
        Command::Stats => stats(&store),
    }
}

fn stats(store: &Store) -> Result<()> {
    println!("{:<14} {:<11} {:>8} {:>7}", "repo", "version", "commits", "merges");
    for s in store.stats()? {
        println!("{:<14} {:<11} {:>8} {:>7}", s.repo, s.version, s.commits, s.merges);
    }
    Ok(())
}
